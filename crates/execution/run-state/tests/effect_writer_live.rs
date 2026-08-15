//! Native private-writer gate over a throwaway PostgreSQL database.

#![cfg(feature = "native")]

use std::time::SystemTime;

use tokio_postgres::{Client, NoTls};
use url::Url;
use wamn_run_state::{
    BeginEffectAttempt, CredentialGeneration, EffectAttemptId, EffectWriterCredentialScope,
    EffectWriterCredentialValidity, EffectWriterErrorKind, EffectWriterScope, RecordEffectOutcome,
    ResetProjectionFence, RunProjectionFence, RunProjectionOutcome, RunProjectionPersistence,
    effect_writer_credential, effect_writer_generation_role,
};

const EMPTY_HASH: &str = "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";
const WRITER_PASSWORD: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

async fn connect(url: &str) -> (Client, tokio::task::JoinHandle<()>) {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to throwaway PostgreSQL");
    let task = tokio::spawn(async move {
        connection.await.expect("drive PostgreSQL connection");
    });
    (client, task)
}

fn attempt(input_ref: &'static str) -> BeginEffectAttempt<'static> {
    attempt_at("writer-run", "effect-node", input_ref)
}

fn attempt_at(
    run_id: &'static str,
    local_node_id: &'static str,
    input_ref: &'static str,
) -> BeginEffectAttempt<'static> {
    BeginEffectAttempt {
        run_id,
        root_plan_hash: EMPTY_HASH,
        current_plan_hash: EMPTY_HASH,
        frame_id: 0,
        parent_frame_id: None,
        call_site_id: None,
        local_node_id,
        source_artifact_hash: EMPTY_HASH,
        requirement_name: "manager",
        occurrence: 0,
        seq: 1,
        generation_fact_kind: "not-required",
        connection_name: None,
        connection_generation: None,
        credential_generation: None,
        verified_author_principal: None,
        verified_publisher_principal: None,
        attempt_deadline_at: "2099-01-01T00:00:00Z",
        attempt_input_ref: input_ref,
    }
}

fn projection_fact(output_port: &'static str) -> RunProjectionPersistence<'static> {
    RunProjectionPersistence {
        fence: RunProjectionFence {
            run_id: "writer-run",
            lease_owner: "writer-live",
            lease_generation: 0,
        },
        current_plan_hash: EMPTY_HASH,
        frame_id: 0,
        parent_frame_id: None,
        call_site_id: None,
        local_node_id: "pure-node",
        occurrence: 0,
        sequence: 3_000_000_000,
        outcome: RunProjectionOutcome::Success { output_port },
        output_json: Some("{\"ok\":true}"),
        input_json: Some("{\"input\":true}"),
        output_size: Some(11),
        payload_hash: Some("0123456789abcdef"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires WAMN_RUN_STORE_PG_URL and a throwaway PostgreSQL database"]
async fn native_effect_writer_live() {
    let admin_url = std::env::var("WAMN_RUN_STORE_PG_URL")
        .expect("set WAMN_RUN_STORE_PG_URL to a throwaway PostgreSQL database");
    let (admin, admin_task) = connect(&admin_url).await;
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await
        .expect("read database identity")
        .get(0);
    let credential_scope = EffectWriterCredentialScope {
        org: "writer-live-org".to_string(),
        project: "writer-live-project".to_string(),
        environment: "writer-live-env".to_string(),
        database: database.clone(),
    };
    let generation_role = effect_writer_generation_role(
        &credential_scope.org,
        &credential_scope.project,
        &credential_scope.environment,
        &credential_scope.database,
        CredentialGeneration::A,
    );
    let role_identifier = quote_identifier(&generation_role);
    let role_literal = quote_literal(&generation_role);
    let password_literal = quote_literal(WRITER_PASSWORD);
    let database_identifier = quote_identifier(&database);
    admin
        .batch_execute(&format!(
            "DO $roles$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
                 CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB \
                   NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_effect_writer') THEN \
                 CREATE ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB \
                   NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_run_projection_writer') THEN \
                 CREATE ROLE wamn_run_projection_writer NOLOGIN NOSUPERUSER NOCREATEDB \
                   NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname={role_literal}) THEN \
                 CREATE ROLE {role_identifier} LOGIN PASSWORD {password_literal} \
                   NOSUPERUSER NOCREATEDB NOCREATEROLE INHERIT NOREPLICATION NOBYPASSRLS; \
               ELSE \
                 ALTER ROLE {role_identifier} LOGIN PASSWORD {password_literal} \
                   NOSUPERUSER NOCREATEDB NOCREATEROLE INHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
             END $roles$; \
             ALTER ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB \
               NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
             GRANT wamn_effect_writer TO {role_identifier}; \
             GRANT wamn_run_projection_writer TO {role_identifier}; \
             REVOKE CONNECT ON DATABASE {database_identifier} FROM PUBLIC, wamn_effect_writer; \
             GRANT CONNECT ON DATABASE {database_identifier} TO {role_identifier}; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE;"
        ))
        .await
        .expect("prepare private writer authority");
    admin
        .batch_execute(include_str!("../../../../deploy/sql/catalog-schema.sql"))
        .await
        .expect("apply catalog schema");
    admin
        .batch_execute(include_str!("../../../../deploy/sql/run-state.sql"))
        .await
        .expect("apply run-state schema");
    admin
        .batch_execute(include_str!("../../../../deploy/sql/run-queue.sql"))
        .await
        .expect("apply run-queue schema");
    admin
        .batch_execute(&format!(
            "INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,state) \
             VALUES ('tenant-live-a','writer-catalog',1,'test','0.1','draft'); \
             INSERT INTO catalog.execution_bundles \
               (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
             VALUES ('tenant-live-a','{EMPTY_HASH}','0.1',decode('7b7d','hex'),2); \
             INSERT INTO catalog.release_manifests \
               (tenant_id,catalog_id,catalog_version,members_json) \
             VALUES ('tenant-live-a','writer-catalog',1,'[]'); \
             INSERT INTO wamn_run.runs \
               (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
                environment,execution_bundle_hash,status) \
             VALUES ('tenant-live-a','writer-run','root',1,'writer-catalog',1, \
                     'test','{EMPTY_HASH}','running'); \
             INSERT INTO wamn_run.run_queue \
               (tenant_id,run_id,lease_owner,lease_expires_at) \
             VALUES ('tenant-live-a','writer-run','writer-live','2099-01-01');"
        ))
        .await
        .expect("seed one actively leased writer run");

    let mut writer_url = Url::parse(&admin_url).expect("parse admin URL");
    writer_url
        .set_username(&generation_role)
        .expect("set generation username");
    writer_url
        .set_password(Some(WRITER_PASSWORD))
        .expect("set generation password");
    writer_url.set_query(None);
    writer_url.set_fragment(None);
    let validity = EffectWriterCredentialValidity {
        issued_at: "2020-01-01T00:00:00Z".to_string(),
        not_before: "2020-01-01T00:00:00Z".to_string(),
        expires_at: "2099-01-01T00:00:00Z".to_string(),
        revoked_at: None,
    };
    let credential = effect_writer_credential(
        &credential_scope,
        "0123456789abcdef0123456789abcdef",
        CredentialGeneration::A,
        &validity,
        writer_url.as_str(),
    );
    let document = serde_json::to_vec(&credential).expect("encode strict credential");
    let host_scope = EffectWriterScope {
        tenant_id: "tenant-live-a",
        org: &credential_scope.org,
        project: &credential_scope.project,
        environment: &credential_scope.environment,
        database: &credential_scope.database,
        schema: "wamn_run",
    };
    let writer = wamn_run_state::EffectWriterClient::from_secret_document(
        &document,
        host_scope,
        SystemTime::now(),
    )
    .await
    .expect("authenticate and retain private writer pool");
    let retained_session: bool = admin
        .query_one(
            "SELECT EXISTS (SELECT FROM pg_stat_activity WHERE usename=$1)",
            &[&generation_role],
        )
        .await
        .expect("observe retained authenticated session")
        .get(0);
    assert!(retained_session);

    let (first_projection, second_projection) = tokio::join!(
        writer.record_run_projection(projection_fact("main")),
        writer.record_run_projection(projection_fact("main")),
    );
    first_projection.expect("first trusted projection");
    second_projection.expect("concurrent exact projection retry");
    let divergent_projection = writer
        .record_run_projection(projection_fact("alternate"))
        .await
        .expect_err("divergent terminal projection retry refuses");
    assert_eq!(
        divergent_projection.kind(),
        EffectWriterErrorKind::DivergentProjection
    );
    let projected = admin
        .query_one(
            "SELECT status, output_port, seq FROM wamn_run.node_runs \
              WHERE tenant_id='tenant-live-a' AND run_id='writer-run' \
                AND frame_id=0 AND local_node_id='pure-node' AND occurrence=0",
            &[],
        )
        .await
        .expect("read trusted projection");
    assert_eq!(projected.get::<_, String>(0), "success");
    assert_eq!(projected.get::<_, String>(1), "main");
    assert_eq!(projected.get::<_, i64>(2), 3_000_000_000);

    let (first, second) = tokio::join!(
        writer.begin_attempt(attempt("sha256:writer-input")),
        writer.begin_attempt(attempt("sha256:writer-input")),
    );
    let first = first.expect("first concurrent attempt");
    let second = second.expect("second concurrent attempt retry");
    assert_eq!(
        first, second,
        "concurrent retry reuses server identity and time"
    );

    let divergent = writer
        .begin_attempt(attempt("sha256:different-input"))
        .await
        .expect_err("divergent attempt retry refuses");
    assert_eq!(divergent.kind(), EffectWriterErrorKind::DivergentRetry);

    let attempt_has_no_run_fk: bool = admin
        .query_one(
            "SELECT NOT EXISTS ( \
                SELECT 1 FROM pg_constraint \
                 WHERE conrelid='wamn_run.effect_attempts'::regclass AND contype='f')",
            &[],
        )
        .await
        .expect("inspect canonical independent ledger")
        .get(0);
    assert!(attempt_has_no_run_fk);

    let missing = EffectAttemptId {
        attempt_id: "00000000-0000-0000-0000-000000000998",
    };
    let error = writer
        .acquire_dispatch(missing)
        .await
        .expect_err("missing attempt cannot produce a permit");
    assert_eq!(error.kind(), EffectWriterErrorKind::MissingAttempt);
    let error = writer
        .record_outcome(RecordEffectOutcome {
            attempt: missing,
            outcome_status: "success",
        })
        .await
        .expect_err("missing dispatch cannot accept an outcome");
    assert_eq!(error.kind(), EffectWriterErrorKind::MissingDispatch);

    let identity = EffectAttemptId {
        attempt_id: &first.attempt_id,
    };
    let (first_dispatch, second_dispatch) = tokio::join!(
        writer.acquire_dispatch(identity),
        writer.acquire_dispatch(identity),
    );
    let permits = [
        first_dispatch.expect("first dispatch race"),
        second_dispatch.expect("second dispatch race"),
    ];
    assert_eq!(
        permits.iter().filter(|permit| permit.is_some()).count(),
        1,
        "only INSERT RETURNING is a dispatch permit"
    );

    let outcome = RecordEffectOutcome {
        attempt: identity,
        outcome_status: "success",
    };
    let (first_outcome, second_outcome) = tokio::join!(
        writer.record_outcome(outcome),
        writer.record_outcome(outcome)
    );
    assert_eq!(
        first_outcome.expect("first concurrent outcome"),
        second_outcome.expect("second concurrent outcome retry"),
        "concurrent outcome retry reuses server timestamps"
    );
    let divergent = writer
        .record_outcome(RecordEffectOutcome {
            attempt: identity,
            outcome_status: "error",
        })
        .await
        .expect_err("divergent outcome retry refuses");
    assert_eq!(divergent.kind(), EffectWriterErrorKind::DivergentRetry);

    admin
        .execute(
            "UPDATE wamn_run.run_queue SET lease_expires_at='2000-01-01' \
              WHERE tenant_id='tenant-live-a' AND run_id='writer-run'",
            &[],
        )
        .await
        .expect("expire effect-bearing reset candidate");
    let prior_expiry: String = admin
        .query_one(
            "SELECT lease_expires_at::text FROM wamn_run.run_queue \
              WHERE tenant_id='tenant-live-a' AND run_id='writer-run'",
            &[],
        )
        .await
        .expect("read exact prior expiry")
        .get(0);
    let effect_won = writer
        .reset_expired_pre_effect_projection(ResetProjectionFence {
            run_id: "writer-run",
            prior_lease_owner: "writer-live",
            prior_lease_expires_at: &prior_expiry,
            prior_lease_generation: 0,
        })
        .await
        .expect_err("immutable effect evidence must defeat projection reset");
    assert_eq!(
        effect_won.kind(),
        EffectWriterErrorKind::EffectAttemptPresent
    );
    assert_eq!(
        admin
            .query_one(
                "SELECT count(*) FROM wamn_run.node_runs \
                  WHERE tenant_id='tenant-live-a' AND run_id='writer-run'",
                &[],
            )
            .await
            .expect("count preserved projection")
            .get::<_, i64>(0),
        1
    );

    admin
        .batch_execute(&format!(
            "INSERT INTO wamn_run.runs \
               (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
                environment,execution_bundle_hash,status,state_json) VALUES \
             ('tenant-live-a','reset-fence','root',1,'writer-catalog',1,'test','{EMPTY_HASH}', \
              'running','{{\"checkpoint\":1}}'), \
             ('tenant-live-a','reset-future','root',1,'writer-catalog',1,'test','{EMPTY_HASH}', \
              'running','{{\"checkpoint\":1}}'), \
             ('tenant-live-a','reset-status','root',1,'writer-catalog',1,'test','{EMPTY_HASH}', \
              'completed','{{\"checkpoint\":1}}'), \
             ('tenant-live-a','reset-duplicate','root',1,'writer-catalog',1,'test','{EMPTY_HASH}', \
              'running','{{\"checkpoint\":1}}'), \
             ('tenant-live-a','reset-gap','root',1,'writer-catalog',1,'test','{EMPTY_HASH}', \
              'running','{{\"checkpoint\":1}}'); \
             INSERT INTO wamn_run.run_queue \
               (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) VALUES \
             ('tenant-live-a','reset-fence','prior-owner','2000-01-01',7), \
             ('tenant-live-a','reset-future','prior-owner','2099-01-01',7), \
             ('tenant-live-a','reset-status','prior-owner','2000-01-01',7), \
             ('tenant-live-a','reset-duplicate','prior-owner','2000-01-01',7), \
             ('tenant-live-a','reset-gap','prior-owner','2000-01-01',7); \
             INSERT INTO wamn_run.node_runs \
               (tenant_id,run_id,frame_id,current_plan_hash,local_node_id,occurrence,seq,status) \
             SELECT 'tenant-live-a',run_id,0,'{EMPTY_HASH}','pure-node',0,1,'started' \
               FROM wamn_run.runs WHERE run_id LIKE 'reset-%';"
        ))
        .await
        .expect("seed reset crash, concurrency, and fence matrix");
    let expired = admin
        .query_one(
            "SELECT lease_expires_at::text FROM wamn_run.run_queue \
              WHERE tenant_id='tenant-live-a' AND run_id='reset-fence'",
            &[],
        )
        .await
        .expect("read exact expired fence")
        .get::<_, String>(0);
    for (label, fence) in [
        (
            "owner",
            ResetProjectionFence {
                run_id: "reset-fence",
                prior_lease_owner: "other-owner",
                prior_lease_expires_at: &expired,
                prior_lease_generation: 7,
            },
        ),
        (
            "expiry",
            ResetProjectionFence {
                run_id: "reset-fence",
                prior_lease_owner: "prior-owner",
                prior_lease_expires_at: "1999-01-01 00:00:00+00",
                prior_lease_generation: 7,
            },
        ),
        (
            "generation-aba",
            ResetProjectionFence {
                run_id: "reset-fence",
                prior_lease_owner: "prior-owner",
                prior_lease_expires_at: &expired,
                prior_lease_generation: 6,
            },
        ),
        (
            "status",
            ResetProjectionFence {
                run_id: "reset-status",
                prior_lease_owner: "prior-owner",
                prior_lease_expires_at: &expired,
                prior_lease_generation: 7,
            },
        ),
        (
            "not-yet-expired",
            ResetProjectionFence {
                run_id: "reset-future",
                prior_lease_owner: "prior-owner",
                prior_lease_expires_at: "2099-01-01 00:00:00+00",
                prior_lease_generation: 7,
            },
        ),
    ] {
        let refusal = writer
            .reset_expired_pre_effect_projection(fence)
            .await
            .unwrap_or_else(|error| {
                assert_eq!(
                    error.kind(),
                    EffectWriterErrorKind::ResetFenceLost,
                    "{label}"
                );
                0
            });
        assert_eq!(refusal, 0, "{label} fence unexpectedly deleted projection");
    }
    let fenced_rows: i64 = admin
        .query_one(
            "SELECT count(*) FROM wamn_run.node_runs WHERE run_id IN \
              ('reset-fence','reset-future','reset-status')",
            &[],
        )
        .await
        .expect("read preserved fence-refusal projections")
        .get(0);
    assert_eq!(fenced_rows, 3);

    let duplicate_fence = ResetProjectionFence {
        run_id: "reset-duplicate",
        prior_lease_owner: "prior-owner",
        prior_lease_expires_at: &expired,
        prior_lease_generation: 7,
    };
    let (first_reset, second_reset) = tokio::join!(
        writer.reset_expired_pre_effect_projection(duplicate_fence),
        writer.reset_expired_pre_effect_projection(duplicate_fence),
    );
    let mut deleted = [
        first_reset.expect("first duplicate resetter"),
        second_reset.expect("second duplicate resetter"),
    ];
    deleted.sort_unstable();
    assert_eq!(
        deleted,
        [0, 1],
        "duplicate resetters serialize idempotently"
    );
    let after_delete_crash = admin
        .query_one(
            "SELECT state_json::text, \
                    (SELECT count(*) FROM wamn_run.node_runs n \
                      WHERE n.tenant_id=r.tenant_id AND n.run_id=r.run_id) \
               FROM wamn_run.runs r WHERE run_id='reset-duplicate'",
            &[],
        )
        .await
        .expect("observe crash immediately after private delete commit");
    assert_eq!(
        after_delete_crash.get::<_, String>(0),
        "{\"checkpoint\": 1}"
    );
    assert_eq!(after_delete_crash.get::<_, i64>(1), 0);

    admin
        .execute(
            &format!(
                "INSERT INTO wamn_run.effect_attempts \
                   (tenant_id,attempt_id,run_id,root_plan_hash,current_plan_hash,frame_id, \
                    local_node_id,source_artifact_hash,requirement_name,occurrence,seq, \
                    generation_fact_kind,attempt_deadline_at,attempt_input_ref) \
                 VALUES ('tenant-live-a','00000000-0000-0000-0000-000000000777', \
                    'reset-gap','{EMPTY_HASH}','{EMPTY_HASH}',0,'effect-node','{EMPTY_HASH}', \
                    'manager',0,1,'not-required','2099-01-01','sha256:gap')"
            ),
            &[],
        )
        .await
        .expect("commit an effect attempt in the app-to-private handoff gap");
    let gap = writer
        .reset_expired_pre_effect_projection(ResetProjectionFence {
            run_id: "reset-gap",
            prior_lease_owner: "prior-owner",
            prior_lease_expires_at: &expired,
            prior_lease_generation: 7,
        })
        .await
        .expect_err("fresh effect evidence wins the handoff gap");
    assert_eq!(gap.kind(), EffectWriterErrorKind::EffectAttemptPresent);
    let gap_preserved = admin
        .query_one(
            "SELECT state_json IS NOT NULL, EXISTS (SELECT 1 FROM wamn_run.node_runs \
              WHERE run_id='reset-gap') FROM wamn_run.runs WHERE run_id='reset-gap'",
            &[],
        )
        .await
        .expect("read handoff-gap preservation");
    assert!(gap_preserved.get::<_, bool>(0));
    assert!(gap_preserved.get::<_, bool>(1));

    admin
        .batch_execute(
            "UPDATE wamn_run.runs SET status='effect-uncertain' \
               WHERE tenant_id='tenant-live-a' AND run_id='writer-run'; \
             DELETE FROM wamn_run.run_queue \
               WHERE tenant_id='tenant-live-a' AND run_id='writer-run';",
        )
        .await
        .expect("terminalize the run after its immutable attempt");
    assert_eq!(
        writer
            .begin_attempt(attempt("sha256:writer-input"))
            .await
            .expect("exact terminal-run retry remains observable"),
        first
    );
    let inactive_new = writer
        .begin_attempt(attempt_at(
            "writer-run",
            "second-effect-node",
            "sha256:writer-input",
        ))
        .await
        .expect_err("new coordinate after terminalization is refused");
    assert_eq!(inactive_new.kind(), EffectWriterErrorKind::RunNotRunnable);

    let tenant_counts = admin
        .query_one(
            "SELECT count(*) FILTER (WHERE tenant_id='tenant-live-a'), \
                    count(*) FILTER (WHERE tenant_id='tenant-live-b') \
               FROM wamn_run.effect_attempts",
            &[],
        )
        .await
        .expect("prove host-fixed writer tenant");
    assert_eq!(tenant_counts.get::<_, i64>(0), 2);
    assert_eq!(tenant_counts.get::<_, i64>(1), 0);

    admin
        .batch_execute("BEGIN; SET LOCAL ROLE wamn_app; SET LOCAL app.tenant='tenant-live-a'")
        .await
        .expect("enter ordinary application authority");
    let ordinary_insert = admin
        .execute(
            "INSERT INTO wamn_run.effect_attempt_outcomes \
               (tenant_id,attempt_id,dispatched_at,outcome_status) \
             VALUES ('tenant-live-a','00000000-0000-0000-0000-000000000999',now(),'success')",
            &[],
        )
        .await
        .expect_err("ordinary non-writer append is denied by ledger ACL");
    assert_eq!(
        ordinary_insert
            .as_db_error()
            .expect("typed ACL refusal")
            .code()
            .code(),
        "42501"
    );
    admin
        .batch_execute("ROLLBACK")
        .await
        .expect("leave ordinary application authority");
    for mutation in [
        "INSERT INTO wamn_run.node_runs \
           (tenant_id,run_id,current_plan_hash,frame_id,local_node_id,occurrence,seq,status) \
         VALUES ('tenant-live-a','writer-run','sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a',0,'app-node',0,9,'started')",
        "UPDATE wamn_run.node_runs SET output_port='forged' \
          WHERE tenant_id='tenant-live-a' AND run_id='writer-run'",
        "DELETE FROM wamn_run.node_runs \
          WHERE tenant_id='tenant-live-a' AND run_id='writer-run'",
    ] {
        admin
            .batch_execute("BEGIN; SET LOCAL ROLE wamn_app; SET LOCAL app.tenant='tenant-live-a'")
            .await
            .expect("enter app projection authority probe");
        let refusal = admin
            .execute(mutation, &[])
            .await
            .expect_err("wamn_app retained node projection mutation");
        assert_eq!(
            refusal
                .as_db_error()
                .expect("typed ACL refusal")
                .code()
                .code(),
            "42501"
        );
        admin
            .batch_execute("ROLLBACK")
            .await
            .expect("leave app projection authority probe");
    }

    drop(writer);
    admin
        .batch_execute(&format!(
            "REVOKE CONNECT ON DATABASE {database_identifier} FROM {role_identifier}; \
             REVOKE wamn_effect_writer, wamn_run_projection_writer FROM {role_identifier}; \
             ALTER ROLE {role_identifier} NOLOGIN PASSWORD NULL VALID UNTIL 'epoch';"
        ))
        .await
        .expect("retire disposable writer generation authority");
    drop(admin);
    admin_task.abort();
}
