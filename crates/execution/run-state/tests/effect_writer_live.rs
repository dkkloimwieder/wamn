//! Native private-writer gate over a throwaway PostgreSQL database.

#![cfg(feature = "native")]

use std::time::SystemTime;

use tokio_postgres::{Client, NoTls};
use url::Url;
use wamn_control_provision::{
    WorkloadRoleFamily, WorkloadRoleScope, sql as provision_sql, workload_generation_role,
};
use wamn_run_state::{
    BeginEffectAttempt, CredentialGeneration, EffectAttemptId, EffectWriterCredentialScope,
    EffectWriterCredentialValidity, EffectWriterErrorKind, EffectWriterScope, RecordEffectOutcome,
    effect_writer_credential, effect_writer_generation_role,
};

const EMPTY_HASH: &str = "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";
const WRITER_PASSWORD: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const APP_GENERATION_PASSWORD: &str = "effect-writer-live-app-0123456789abcdef0123456789abcdef";
const APP_GENERATION_EXPIRES_AT: &str = "2099-01-01T00:00:00Z";

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
        tenant: "tenant-live-a".to_string(),
        org: "writer-live-org".to_string(),
        project: "writer-live-project".to_string(),
        environment: "writer-live-env".to_string(),
        database: database.clone(),
    };
    let generation_role = effect_writer_generation_role(
        &credential_scope.tenant,
        &credential_scope.database,
        CredentialGeneration::A,
    );
    let app_generation_role = workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant {
            tenant: "tenant-live-a",
            database: &database,
        },
        CredentialGeneration::A,
    )
    .expect("derive ordinary App generation");
    let role_identifier = quote_identifier(&generation_role);
    let app_role_identifier = quote_identifier(&app_generation_role);
    let role_literal = quote_literal(&generation_role);
    let password_literal = quote_literal(WRITER_PASSWORD);
    let database_identifier = quote_identifier(&database);
    admin
        .batch_execute(&provision_sql::ensure_app_acl_role_sql())
        .await
        .expect("converge stable App ACL role");
    admin
        .batch_execute(&format!(
            "DO $roles$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB \
                   NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_effect_writer') THEN \
                 CREATE ROLE wamn_effect_writer NOLOGIN NOSUPERUSER NOCREATEDB \
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
             REVOKE CONNECT ON DATABASE {database_identifier} FROM PUBLIC, wamn_effect_writer; \
             GRANT CONNECT ON DATABASE {database_identifier} TO {role_identifier}; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE;"
        ))
        .await
        .expect("prepare private writer authority");
    admin
        .batch_execute(&provision_sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::App,
            &database,
            &app_generation_role,
            APP_GENERATION_PASSWORD,
            APP_GENERATION_EXPIRES_AT,
        ))
        .await
        .expect("prepare ordinary App generation");
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
        .batch_execute(
            "INSERT INTO catalog.packages \
               (tenant_id,package_id,package_version,manifest_sha256) \
             VALUES ('tenant-live-a','writer_catalog','1.0.0', \
               'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'); \
             INSERT INTO catalog.effective_releases \
               (tenant_id,effective_release_id,environment,verified_publisher_principal) \
             VALUES ('tenant-live-a',1,'test','test-publisher'); \
             INSERT INTO catalog.effective_release_packages \
               (tenant_id,effective_release_id,package_id,package_version) \
             VALUES ('tenant-live-a',1,'writer_catalog','1.0.0'); \
             INSERT INTO wamn_run.environment_policies \
               (tenant_id,expected_environment,durability_class) \
             VALUES ('tenant-live-a','test','standard'); \
             INSERT INTO wamn_run.runs \
               (tenant_id,run_id,flow_id,flow_version,package_id,effective_release_id, \
                environment,wiring_id,wiring_version,status) \
             VALUES ('tenant-live-a','writer-run','root',1,'writer_catalog',1, \
                     'test','writer-wiring',1,'running'); \
             INSERT INTO wamn_run.run_queue \
               (tenant_id,run_id,lease_owner,lease_expires_at) \
             VALUES ('tenant-live-a','writer-run','writer-live','2099-01-01');",
        )
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
    assert_eq!(tenant_counts.get::<_, i64>(0), 1);
    assert_eq!(tenant_counts.get::<_, i64>(1), 0);

    let mut app_url = Url::parse(&admin_url).expect("parse admin URL for ordinary App generation");
    app_url
        .set_username(&app_generation_role)
        .expect("set ordinary App-generation username");
    app_url
        .set_password(Some(APP_GENERATION_PASSWORD))
        .expect("set ordinary App-generation password");
    app_url.set_query(None);
    app_url.set_fragment(None);
    let (app, app_task) = connect(app_url.as_str()).await;
    app.batch_execute("SET app.tenant='tenant-live-a'")
        .await
        .expect("set ordinary App tenant row input");
    let app_identity = app
        .query_one(
            "SELECT current_user::text, \
                    wamn_authority.current_tenant_key() = \
                        wamn_authority.tenant_key('tenant-live-a'), \
                    has_table_privilege( \
                        current_user, 'wamn_run.effect_attempt_outcomes', 'INSERT')",
            &[],
        )
        .await
        .expect("inspect ordinary App-generation ledger ACL");
    assert_eq!(app_identity.get::<_, String>(0), app_generation_role);
    assert!(app_identity.get::<_, bool>(1));
    assert!(!app_identity.get::<_, bool>(2));
    let ordinary_insert = app
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
    drop(app);
    let _ = app_task.await;
    admin
        .batch_execute(&provision_sql::retire_workload_generation_sql(
            WorkloadRoleFamily::App,
            &database,
            &app_generation_role,
        ))
        .await
        .expect("retire ordinary App generation");
    admin
        .batch_execute(&format!("DROP ROLE {app_role_identifier}"))
        .await
        .expect("drop ordinary App generation");
    drop(writer);
    admin
        .batch_execute(&format!(
            "REVOKE CONNECT ON DATABASE {database_identifier} FROM {role_identifier}; \
             REVOKE wamn_effect_writer FROM {role_identifier}; \
             ALTER ROLE {role_identifier} NOLOGIN PASSWORD NULL VALID UNTIL 'epoch';"
        ))
        .await
        .expect("retire disposable writer generation authority");
    drop(admin);
    admin_task.abort();
}
