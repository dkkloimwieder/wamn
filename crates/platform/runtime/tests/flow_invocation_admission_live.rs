//! Live proofs for the PostgreSQL invocation and admission boundary.
//!
//! Run this only against a disposable PostgreSQL 18 database. The fixture
//! recreates the canonical `catalog` and `wamn_run` schemas and creates the
//! cluster-wide roles those schemas require.

use anyhow::Context as _;
use chrono::{TimeDelta, Utc};
use serde_json::json;
use tokio_postgres::{Client, NoTls};
use wamn_flow_invocation::{BeginResult, InvocationError, InvokeRequest};
use wamn_run_state::invocation::InvocationTarget;
use wamn_runtime::flow_invocation::{
    HttpAdmission, InvocationBackend, InvocationService, InvocationServiceConfig,
    PostgresInvocationBackend,
};

const TENANT: &str = "d15-write-ahead";
const CATALOG: &str = "d15-catalog";
const ENVIRONMENT: &str = "dev";
const ATTACHMENT: &str = "http-a";
const CRON_ATTACHMENT: &str = "cron-a";
const ABSENT_ATTACHMENT: &str = "absent-a";
const FLOW: &str = "flow-http";
const DEFINITION_HASH: &str = "sha256:http";

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("D15 admission connection failed: {error}");
        }
    });
    Ok(client)
}

async fn install_fixture(client: &Client) -> anyhow::Result<()> {
    let catalog_ddl = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../deploy/sql/catalog-schema.sql"
    ));
    let run_state_ddl = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../deploy/sql/run-state.sql"
    ));
    let run_queue_ddl = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../deploy/sql/run-queue.sql"
    ));
    client
        .batch_execute(&format!(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' \
                   NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
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
             END $$; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             {catalog_ddl} {run_state_ddl} {run_queue_ddl}"
        ))
        .await
        .context("install canonical admission schemas")?;

    client
        .batch_execute(&format!(
            "INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,state) \
             VALUES ('{TENANT}','{CATALOG}',1,'{ENVIRONMENT}','0.1','applied'); \
             INSERT INTO catalog.flow_artifacts \
               (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash,artifact_hash) \
             VALUES ('{TENANT}','{FLOW}',1,'0.1','{{}}','graph-hash','artifact-hash'); \
             INSERT INTO catalog.execution_bundles \
               (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
             VALUES ('{TENANT}', \
               'sha256:' || encode(sha256(convert_to('{{}}','UTF8')), 'hex'), \
               '0.1',convert_to('{{}}','UTF8'),2); \
             INSERT INTO catalog.release_manifests \
               (tenant_id,catalog_id,catalog_version) \
             VALUES ('{TENANT}','{CATALOG}',1); \
             INSERT INTO catalog.release_flows \
               (tenant_id,catalog_id,catalog_version,flow_id,flow_version,execution_bundle_hash) \
             VALUES ('{TENANT}','{CATALOG}',1,'{FLOW}',1, \
               'sha256:' || encode(sha256(convert_to('{{}}','UTF8')), 'hex')); \
             INSERT INTO catalog.release_exposure_manifests \
               (tenant_id,catalog_id,catalog_version,definitions_json) \
             VALUES ('{TENANT}','{CATALOG}',1,'{{}}'); \
             INSERT INTO catalog.release_sources \
               (tenant_id,catalog_id,catalog_version,source_id,source_kind,definition_json,source_hash) \
             VALUES ('{TENANT}','{CATALOG}',1,'auth-a','auth','{{}}','source-http'); \
             INSERT INTO catalog.release_attachments \
               (tenant_id,catalog_id,catalog_version,attachment_id,attachment_kind,flow_id, \
                source_id,definition_hash,definition_json,route_host,route_path,route_template, \
                route_method) \
             VALUES ('{TENANT}','{CATALOG}',1,'{ATTACHMENT}','http','{FLOW}','auth-a', \
               '{DEFINITION_HASH}','{{}}','example.test','/echo','/echo','POST'); \
             INSERT INTO catalog.release_attachments \
               (tenant_id,catalog_id,catalog_version,attachment_id,attachment_kind,flow_id, \
                source_id,definition_hash,definition_json) \
             VALUES ('{TENANT}','{CATALOG}',1,'{CRON_ATTACHMENT}','cron','{FLOW}','auth-a', \
               '{DEFINITION_HASH}','{{\"run-deadline-ms\":60000}}'); \
             INSERT INTO catalog.catalog_heads \
               (tenant_id,catalog_id,environment,applied_catalog_version) \
             VALUES ('{TENANT}','{CATALOG}','{ENVIRONMENT}',1); \
             INSERT INTO catalog.attachment_activation \
               (tenant_id,catalog_id,environment,attachment_id,confirmed_definition_hash,enabled) \
             VALUES ('{TENANT}','{CATALOG}','{ENVIRONMENT}','{ATTACHMENT}', \
               '{DEFINITION_HASH}',true), \
                    ('{TENANT}','{CATALOG}','{ENVIRONMENT}','{CRON_ATTACHMENT}', \
               '{DEFINITION_HASH}',true);"
        ))
        .await
        .context("seed the active release")?;

    Ok(())
}

async fn install_commit_fault(client: &Client) -> anyhow::Result<()> {
    client
        .batch_execute(
            "CREATE FUNCTION wamn_run.fail_d15_admission_commit() \
               RETURNS trigger LANGUAGE plpgsql AS $$ \
               BEGIN \
                 RAISE EXCEPTION USING ERRCODE = '40001', \
                   MESSAGE = 'injected-admission-commit-failure'; \
               END $$; \
             CREATE CONSTRAINT TRIGGER fail_d15_admission_commit \
               AFTER INSERT ON wamn_run.run_queue \
               DEFERRABLE INITIALLY DEFERRED \
               FOR EACH ROW EXECUTE FUNCTION wamn_run.fail_d15_admission_commit();",
        )
        .await
        .context("install deferred admission commit fault")?;
    Ok(())
}

fn admission() -> HttpAdmission {
    HttpAdmission {
        target: InvocationTarget {
            catalog_version: 1,
            definition_hash: DEFINITION_HASH.to_string(),
            flow_id: FLOW.to_string(),
            flow_version: 1,
            definition: json!({}),
            auth_policy: json!({}),
            enabled: true,
            idempotency_required: true,
        },
        request: InvokeRequest {
            attachment_id: ATTACHMENT.to_string(),
            expected_catalog_version: 1,
            expected_definition_hash: DEFINITION_HASH.to_string(),
            client_request_fingerprint: "sha256:request".to_string(),
            payload: r#"{"request":1}"#.to_string(),
            idempotency_key: Some("client-key".to_string()),
            principal: "principal".to_string(),
            deadline_override: None,
            trace: None,
        },
        principal_digest: "sha256:principal".to_string(),
        client_key_digest: Some("sha256:client-key".to_string()),
        input: json!({"request": 1}),
        invocation_context: json!({"request-id": "d15"}),
        response_deadline_at: Some(Utc::now() + TimeDelta::seconds(30)),
        run_deadline_at: Utc::now() + TimeDelta::minutes(1),
    }
}

fn config() -> InvocationServiceConfig {
    InvocationServiceConfig {
        tenant_id: TENANT.to_string(),
        catalog_id: CATALOG.to_string(),
        environment: ENVIRONMENT.to_string(),
        platform_revision: "d15-test".to_string(),
    }
}

async fn refusal_for(
    service: &InvocationService<PostgresInvocationBackend>,
    attachment_id: &str,
) -> anyhow::Result<(u16, Vec<u8>)> {
    let mut request = admission().request;
    request.attachment_id = attachment_id.to_string();
    let BeginResult::Rejected(rejection) = service.begin(request).await? else {
        anyhow::bail!("a non-callable attachment must not be admitted");
    };
    Ok((rejection.status, rejection.code.into_bytes()))
}

#[tokio::test]
#[ignore = "requires WAMN_FLOW_INVOCATION_PG_URL and a disposable PostgreSQL 18 database"]
async fn cron_attachment_and_absent_path_have_indistinguishable_not_found_responses()
-> anyhow::Result<()> {
    let url = std::env::var("WAMN_FLOW_INVOCATION_PG_URL")
        .context("set WAMN_FLOW_INVOCATION_PG_URL to a disposable PostgreSQL 18 database")?;
    let admin = connect(&url).await?;
    install_fixture(&admin).await?;

    let service = InvocationService::new(
        PostgresInvocationBackend::from_database_url(&url)?,
        config(),
    );
    let cron = refusal_for(&service, CRON_ATTACHMENT).await?;
    let absent = refusal_for(&service, ABSENT_ATTACHMENT).await?;

    assert_eq!(cron, (404, b"attachment-not-found".to_vec()));
    assert_eq!(absent, (404, b"attachment-not-found".to_vec()));
    assert_eq!(
        cron, absent,
        "the anonymous callable surface must not disclose that the cron attachment exists"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires WAMN_FLOW_INVOCATION_PG_URL and a disposable PostgreSQL 18 database"]
async fn commit_failure_returns_no_admission_identity_or_rows() -> anyhow::Result<()> {
    let url = std::env::var("WAMN_FLOW_INVOCATION_PG_URL")
        .context("set WAMN_FLOW_INVOCATION_PG_URL to a disposable PostgreSQL 18 database")?;
    let admin = connect(&url).await?;
    install_fixture(&admin).await?;
    install_commit_fault(&admin).await?;

    let backend = PostgresInvocationBackend::from_database_url(&url)?;
    let config = config();
    let failure = backend
        .admit(&config, &admission())
        .await
        .expect_err("a commit failure must not return an admission identity");
    assert_eq!(failure.kind(), InvocationError::StoreUnavailable);
    assert!(
        failure.to_string().contains("commit admission transaction"),
        "the injected deferred fault must reach the production commit boundary: {failure}"
    );

    let row = admin
        .query_one(
            "SELECT \
               (SELECT count(*) FROM wamn_run.runs), \
               (SELECT count(*) FROM wamn_run.run_queue), \
               (SELECT count(*) FROM wamn_run.invocation_admissions)",
            &[],
        )
        .await?;
    assert_eq!(row.get::<_, i64>(0), 0, "no run committed");
    assert_eq!(row.get::<_, i64>(1), 0, "no queue row committed");
    assert_eq!(row.get::<_, i64>(2), 0, "no producer identity committed");
    Ok(())
}
