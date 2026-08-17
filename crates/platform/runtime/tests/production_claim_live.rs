use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Context as _;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio_postgres::{Client, NoTls};
use url::Url;
use wamn_run_state::{
    BeginEffectAttempt, CredentialGeneration, EffectAttempt, EffectWriterClient,
    EffectWriterCredentialScope, EffectWriterCredentialValidity, EffectWriterErrorKind,
    EffectWriterScope, FailKind, ResetProjectionFence, RunStatus, effect_writer_credential,
    effect_writer_generation_role, queue::select_production_claim_sql,
};
use wamn_runtime::plugins::wamn_postgres::{
    ProductionClaimErrorKind, ProductionClaimResult, ProductionReapResult, WamnPostgres,
    WamnPostgresConfig,
};

const TENANT: &str = "claim-live";
const COMPONENT: &str = "claim-live-runner";
/// A second pod, carrying a DIFFERENT release — the rollout case.
const ROLLED_COMPONENT: &str = "claim-live-runner-next";
const SCHEMA: &str = "wamn_claim_live";
/// The release the claiming pod carries. Deliberately distinct from every
/// catalog version this fixture publishes, so a recorded pair that matched the
/// admitted or the republished release instead of the pod would be visible.
const POD_RELEASE_VERSION: i32 = 7;
const POD_MANIFEST_DIGEST: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const ROLLED_RELEASE_VERSION: i32 = 8;
const ROLLED_MANIFEST_DIGEST: &str =
    "sha256:2222222222222222222222222222222222222222222222222222222222222222";
const RELEASE_ARTIFACT: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const EMPTY_HASH: &str = "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";
const WRITER_PASSWORD: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const WRITER_LATCH: i64 = 7_141_013;
const PRIOR_WINNER_HASH: &str = "sha256:prior-caller-winner";

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("production-claim live connection failed: {error}");
        }
    });
    Ok(client)
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn effect_attempt(
    run_id: &'static str,
    local_node_id: &'static str,
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
        attempt_input_ref: "sha256:claim-live-effect-input",
    }
}

fn plan_bytes(root_artifact_hash: &str) -> (String, Vec<u8>) {
    let guard = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object"
    });
    let body = json!({
        "header": {
            "format-version": "0.1",
            "plan-compiler-revision": "0.1",
            "runtime-revision": {
                "flowrunner-component-digest": format!("sha256:{}", "c".repeat(64)),
                "effect-provider-revision": format!("sha256:{}", "d".repeat(64)),
                "host-effect-contract-version": "0.1"
            },
            "root-artifact-hash": root_artifact_hash
        },
        "body": {
            "entry-instruction": "request",
            "nodes": [
                {
                    "local-node-id": "request",
                    "source-node-id": "request",
                    "type": "request",
                    "config": {"input-schema": guard.clone()},
                    "effect-policy": "pure"
                },
                {
                    "local-node-id": "respond",
                    "source-node-id": "respond",
                    "type": "respond",
                    "config": {"status": 200},
                    "effect-policy": "pure"
                }
            ],
            "edges": [{
                "source": "request",
                "source-port": "main",
                "destination": "respond",
                "fan-out-ordinal": 0
            }],
            "root-terminal-behavior": {"kind": "respond", "responders": ["respond"]},
            "entry-input-schema-guard": guard.clone(),
            "callable-contract": {
                "version": "0.1",
                "input-schema-hash": wamn_flow::canonical_json_sha256(&guard),
                "return-contract": "untyped-json-body",
                "effect-ceiling": "effectful"
            },
            "source-map": [
                {"local-node-id": "request", "source-node-id": "request"},
                {"local-node-id": "respond", "source-node-id": "respond"}
            ]
        }
    });
    let bytes = serde_json::to_vec(&body).expect("plan fixture serializes");
    (digest(&bytes), bytes)
}

async fn install_schema(client: &Client) -> anyhow::Result<()> {
    client
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             CREATE SCHEMA {SCHEMA}; \
             CREATE SCHEMA catalog; \
             CREATE TABLE {SCHEMA}.runs ( \
               tenant_id text NOT NULL, run_id text NOT NULL, flow_id text NOT NULL, \
               flow_version int NOT NULL, status text NOT NULL, catalog_id text NOT NULL, \
               catalog_version int NOT NULL, environment text NOT NULL, \
               execution_bundle_hash text NOT NULL, input_json jsonb NOT NULL DEFAULT '{{}}', \
               state_json jsonb, invocation_context jsonb NOT NULL DEFAULT '{{}}', \
               trigger_source text, event_source_run_id text, event_root_run_id text, \
               event_depth int, admission_context_version text, platform_revision text, \
               capture_mode text, release_version int, manifest_digest text, \
               idempotency_key text, response_deadline_at timestamptz, \
               run_deadline_at timestamptz, \
               fail_kind text, fail_node text, fail_reason text, \
               caller_outcome_kind text, caller_outcome_json jsonb, caller_http_status int, \
               caller_release_node_id text, caller_outcome_hash text, \
               caller_released_at timestamptz, updated_at timestamptz NOT NULL DEFAULT now(), \
               CONSTRAINT runs_release_record_check CHECK ( \
                 (release_version IS NULL AND manifest_digest IS NULL) \
                 OR (release_version > 0 \
                     AND manifest_digest ~ '^sha256:[0-9a-f]{{64}}$')), \
               PRIMARY KEY (tenant_id, run_id)); \
             CREATE TABLE {SCHEMA}.node_runs ( \
               tenant_id text NOT NULL, run_id text NOT NULL, local_node_id text NOT NULL, \
               PRIMARY KEY (tenant_id, run_id, local_node_id)); \
             CREATE TABLE {SCHEMA}.effect_attempts ( \
               tenant_id text NOT NULL, attempt_id uuid NOT NULL DEFAULT gen_random_uuid(), \
               run_id text NOT NULL, root_plan_hash text NOT NULL, current_plan_hash text NOT NULL, \
               frame_id bigint NOT NULL, parent_frame_id bigint, call_site_id text, \
               local_node_id text NOT NULL, source_artifact_hash text NOT NULL, \
               requirement_name text NOT NULL, occurrence int NOT NULL, seq int NOT NULL, \
               generation_fact_kind text NOT NULL, connection_name text, \
               connection_generation text, credential_generation text, \
               verified_author_principal text, verified_publisher_principal text, \
               attempt_started_at timestamptz NOT NULL DEFAULT clock_timestamp(), \
               attempt_deadline_at timestamptz NOT NULL, attempt_input_ref text NOT NULL, \
               created_at timestamptz NOT NULL DEFAULT clock_timestamp(), \
               PRIMARY KEY (tenant_id, attempt_id), \
               UNIQUE (tenant_id,run_id,frame_id,local_node_id,occurrence)); \
             CREATE FUNCTION {SCHEMA}.guard_run_admission_pins_immutable() \
               RETURNS trigger LANGUAGE plpgsql AS $guard$ \
               BEGIN \
                 IF NEW.catalog_id IS DISTINCT FROM OLD.catalog_id \
                    OR NEW.catalog_version IS DISTINCT FROM OLD.catalog_version \
                    OR NEW.environment IS DISTINCT FROM OLD.environment \
                    OR NEW.execution_bundle_hash IS DISTINCT FROM OLD.execution_bundle_hash \
                    OR NEW.capture_mode IS DISTINCT FROM OLD.capture_mode THEN \
                   RAISE EXCEPTION USING ERRCODE = '55000', \
                     MESSAGE = 'run-admission-pin-immutable'; \
                 END IF; \
                 IF OLD.release_version IS NOT NULL \
                    OR OLD.manifest_digest IS NOT NULL THEN \
                   IF NEW.release_version IS NULL AND NEW.manifest_digest IS NULL THEN \
                     IF NEW.status NOT IN ('dispatched', 'running') \
                        OR EXISTS (SELECT 1 FROM {SCHEMA}.node_runs AS projection \
                                    WHERE projection.tenant_id = OLD.tenant_id \
                                      AND projection.run_id = OLD.run_id) \
                        OR EXISTS (SELECT 1 FROM {SCHEMA}.effect_attempts AS effect \
                                    WHERE effect.tenant_id = OLD.tenant_id \
                                      AND effect.run_id = OLD.run_id) THEN \
                       RAISE EXCEPTION USING ERRCODE = '55000', \
                         MESSAGE = 'run-release-record-immutable'; \
                     END IF; \
                   ELSIF NEW.release_version IS DISTINCT FROM OLD.release_version \
                      OR NEW.manifest_digest IS DISTINCT FROM OLD.manifest_digest THEN \
                     RAISE EXCEPTION USING ERRCODE = '55000', \
                       MESSAGE = 'run-release-record-immutable'; \
                   END IF; \
                 END IF; \
                 RETURN NEW; \
               END $guard$; \
             CREATE TRIGGER runs_admission_pins_immutable \
               BEFORE UPDATE OF catalog_id, catalog_version, environment, \
                                execution_bundle_hash, capture_mode, \
                                release_version, manifest_digest \
               ON {SCHEMA}.runs FOR EACH ROW \
               EXECUTE FUNCTION {SCHEMA}.guard_run_admission_pins_immutable(); \
             CREATE TABLE {SCHEMA}.run_queue ( \
               tenant_id text NOT NULL, run_id text NOT NULL, priority int NOT NULL DEFAULT 0, \
               available_at timestamptz NOT NULL DEFAULT now(), stream_seq bigint NOT NULL DEFAULT 0, \
               lease_owner text, lease_expires_at timestamptz, \
               lease_generation bigint NOT NULL DEFAULT 0, attempts int NOT NULL DEFAULT 0, \
               max_attempts int NOT NULL DEFAULT 3, enqueued_at timestamptz NOT NULL DEFAULT now(), \
               PRIMARY KEY (tenant_id, run_id), \
               FOREIGN KEY (tenant_id, run_id) REFERENCES {SCHEMA}.runs); \
             CREATE TABLE catalog.execution_bundles ( \
               tenant_id text NOT NULL, execution_bundle_hash text NOT NULL, exact_bytes bytea NOT NULL, \
               PRIMARY KEY (tenant_id, execution_bundle_hash)); \
             CREATE TABLE catalog.release_flows ( \
               tenant_id text NOT NULL, catalog_id text NOT NULL, catalog_version int NOT NULL, \
               flow_id text NOT NULL, flow_version int NOT NULL, execution_bundle_hash text NOT NULL, \
               PRIMARY KEY (tenant_id, catalog_id, catalog_version, flow_id)); \
             CREATE TABLE catalog.flow_artifacts ( \
               tenant_id text NOT NULL, flow_id text NOT NULL, flow_version int NOT NULL, \
               artifact_hash text NOT NULL, PRIMARY KEY (tenant_id, flow_id, flow_version)); \
             CREATE TABLE catalog.connection_bindings ( \
               tenant_id text NOT NULL, catalog_id text NOT NULL, catalog_version int NOT NULL, \
               environment text NOT NULL, artifact_hash text NOT NULL, requirement_name text NOT NULL, \
               binding_status text NOT NULL, validation_status text NOT NULL, instance_id text NOT NULL); \
             CREATE TABLE catalog.connection_instances ( \
               tenant_id text NOT NULL, environment text NOT NULL, instance_id text NOT NULL, \
               requirement_type text NOT NULL, contract text NOT NULL, lifecycle_status text NOT NULL, \
               active_generation bigint NOT NULL, PRIMARY KEY (tenant_id, environment, instance_id)); \
             CREATE TABLE catalog.connection_generations ( \
               tenant_id text NOT NULL, environment text NOT NULL, instance_id text NOT NULL, \
               generation bigint NOT NULL, PRIMARY KEY (tenant_id, environment, instance_id, generation));"
        ))
        .await?;
    Ok(())
}

async fn install_effect_writer(
    client: &Client,
    admin_url: &str,
) -> anyhow::Result<(EffectWriterClient, String)> {
    let database: String = client
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    let scope = EffectWriterCredentialScope {
        org: "claim-live-org".to_string(),
        project: "claim-live-project".to_string(),
        environment: "claim-live-env".to_string(),
        database: database.clone(),
    };
    let role = effect_writer_generation_role(
        &scope.org,
        &scope.project,
        &scope.environment,
        &scope.database,
        CredentialGeneration::A,
    );
    let role_identifier = quote_identifier(&role);
    let role_literal = quote_literal(&role);
    let password_literal = quote_literal(WRITER_PASSWORD);
    client
        .batch_execute(&format!(
            "DO $roles$ BEGIN \
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
             GRANT wamn_effect_writer TO {role_identifier}; \
             GRANT wamn_run_projection_writer TO {role_identifier}; \
             GRANT CONNECT ON DATABASE {} TO {role_identifier}; \
             GRANT USAGE ON SCHEMA {SCHEMA} TO wamn_effect_writer; \
             GRANT SELECT (tenant_id,run_id,status) \
               ON {SCHEMA}.runs TO wamn_effect_writer; \
             GRANT SELECT (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) \
               ON {SCHEMA}.run_queue TO wamn_effect_writer; \
             GRANT SELECT,INSERT ON {SCHEMA}.effect_attempts TO wamn_effect_writer; \
             GRANT USAGE ON SCHEMA {SCHEMA} TO wamn_run_projection_writer; \
             GRANT SELECT,INSERT,UPDATE,DELETE ON {SCHEMA}.node_runs \
               TO wamn_run_projection_writer; \
             ALTER TABLE {SCHEMA}.runs ENABLE ROW LEVEL SECURITY; \
             ALTER TABLE {SCHEMA}.runs FORCE ROW LEVEL SECURITY; \
             CREATE POLICY runs_tenant ON {SCHEMA}.runs \
               USING (tenant_id=NULLIF(current_setting('app.tenant',true),'')); \
             ALTER TABLE {SCHEMA}.run_queue ENABLE ROW LEVEL SECURITY; \
             ALTER TABLE {SCHEMA}.run_queue FORCE ROW LEVEL SECURITY; \
             CREATE POLICY run_queue_tenant ON {SCHEMA}.run_queue \
               USING (tenant_id=NULLIF(current_setting('app.tenant',true),'')); \
             ALTER TABLE {SCHEMA}.node_runs ENABLE ROW LEVEL SECURITY; \
             ALTER TABLE {SCHEMA}.node_runs FORCE ROW LEVEL SECURITY; \
             CREATE POLICY node_runs_tenant ON {SCHEMA}.node_runs \
               USING (tenant_id=NULLIF(current_setting('app.tenant',true),'')) \
               WITH CHECK (tenant_id=NULLIF(current_setting('app.tenant',true),'')); \
             ALTER TABLE {SCHEMA}.effect_attempts ENABLE ROW LEVEL SECURITY; \
             ALTER TABLE {SCHEMA}.effect_attempts FORCE ROW LEVEL SECURITY; \
             CREATE POLICY effect_attempts_tenant ON {SCHEMA}.effect_attempts \
               USING (tenant_id=NULLIF(current_setting('app.tenant',true),'')) \
               WITH CHECK (tenant_id=NULLIF(current_setting('app.tenant',true),''));",
            quote_identifier(&database),
        ))
        .await?;

    let mut writer_url = Url::parse(admin_url)?;
    writer_url
        .set_username(&role)
        .map_err(|()| anyhow::anyhow!("set writer username"))?;
    writer_url
        .set_password(Some(WRITER_PASSWORD))
        .map_err(|()| anyhow::anyhow!("set writer password"))?;
    writer_url.set_fragment(None);
    let validity = EffectWriterCredentialValidity {
        issued_at: "2020-01-01T00:00:00Z".to_string(),
        not_before: "2020-01-01T00:00:00Z".to_string(),
        expires_at: "2099-01-01T00:00:00Z".to_string(),
        revoked_at: None,
    };
    let credential = effect_writer_credential(
        &scope,
        "0123456789abcdef0123456789abcdef",
        CredentialGeneration::A,
        &validity,
        writer_url.as_str(),
    );
    let document = serde_json::to_vec(&credential)?;
    let writer = EffectWriterClient::from_secret_document(
        &document,
        EffectWriterScope {
            tenant_id: TENANT,
            org: &scope.org,
            project: &scope.project,
            environment: &scope.environment,
            database: &scope.database,
            schema: SCHEMA,
        },
        SystemTime::now(),
    )
    .await
    .map_err(anyhow::Error::new)?;
    Ok((writer, role))
}

fn url_with_application_name(url: &str, name: &str) -> anyhow::Result<String> {
    let mut parsed = Url::parse(url)?;
    parsed
        .query_pairs_mut()
        .append_pair("application_name", name);
    Ok(parsed.into())
}

async fn wait_for_advisory_wait(
    client: &Client,
    application_name: Option<&str>,
    role: Option<&str>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        application_name.is_some() ^ role.is_some(),
        "select exactly one blocked backend identity"
    );
    for _ in 0..1_000 {
        let waiting: bool = client
            .query_one(
                "SELECT EXISTS ( \
                    SELECT 1 FROM pg_stat_activity \
                     WHERE datname=current_database() \
                       AND ($1::text IS NULL OR application_name=$1) \
                       AND ($2::text IS NULL OR usename=$2) \
                       AND wait_event_type='Lock' AND wait_event='advisory')",
                &[&application_name, &role],
            )
            .await?
            .get(0);
        if waiting {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    anyhow::bail!(
        "backend application={application_name:?} role={role:?} never waited on the advisory lock"
    )
}

async fn seed_release(
    client: &Client,
    catalog_id: &str,
    bundle_hash: &str,
    exact_bytes: &[u8],
) -> anyhow::Result<()> {
    client
        .execute(
            "INSERT INTO catalog.execution_bundles \
               (tenant_id,execution_bundle_hash,exact_bytes) VALUES ($1,$2,$3) \
             ON CONFLICT DO NOTHING",
            &[&TENANT, &bundle_hash, &exact_bytes],
        )
        .await?;
    client
        .execute(
            "INSERT INTO catalog.flow_artifacts \
               (tenant_id,flow_id,flow_version,artifact_hash) VALUES ($1,'root',1,$2) \
             ON CONFLICT DO NOTHING",
            &[&TENANT, &RELEASE_ARTIFACT],
        )
        .await?;
    client
        .execute(
            "INSERT INTO catalog.release_flows \
               (tenant_id,catalog_id,catalog_version,flow_id,flow_version,execution_bundle_hash) \
             VALUES ($1,$2,1,'root',1,$3)",
            &[&TENANT, &catalog_id, &bundle_hash],
        )
        .await?;
    Ok(())
}

async fn seed_run(
    client: &Client,
    run_id: &str,
    catalog_id: &str,
    bundle_hash: &str,
    stream_seq: i64,
) -> anyhow::Result<()> {
    client
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs \
                   (tenant_id,run_id,flow_id,flow_version,status,catalog_id,catalog_version, \
                    environment,execution_bundle_hash,input_json,trigger_source) \
                 VALUES ($1,$2,'root',1,'dispatched',$3,1,'test',$4,'{{\"input\":true}}','http')"
            ),
            &[&TENANT, &run_id, &catalog_id, &bundle_hash],
        )
        .await?;
    client
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.run_queue \
                   (tenant_id,run_id,available_at,stream_seq) \
                 VALUES ($1,$2,'2000-01-01 00:00:00+00',$3)"
            ),
            &[&TENANT, &run_id, &stream_seq],
        )
        .await?;
    Ok(())
}

async fn seed_live_effect_run(
    client: &Client,
    run_id: &str,
    bundle_hash: &str,
    stream_seq: i64,
) -> anyhow::Result<()> {
    seed_run(client, run_id, "cat-main", bundle_hash, stream_seq).await?;
    client
        .execute(
            &format!(
                "WITH running AS ( \
                    UPDATE {SCHEMA}.runs SET status='running' \
                     WHERE tenant_id=$1 AND run_id=$2 \
                     RETURNING tenant_id,run_id) \
                 UPDATE {SCHEMA}.run_queue AS q \
                    SET lease_owner='runner-a', lease_expires_at='2099-01-01' \
                   FROM running \
                  WHERE q.tenant_id=running.tenant_id AND q.run_id=running.run_id"
            ),
            &[&TENANT, &run_id],
        )
        .await?;
    Ok(())
}

async fn expire_effect_run(client: &Client, run_id: &str) -> anyhow::Result<()> {
    client
        .execute(
            &format!(
                "UPDATE {SCHEMA}.run_queue SET lease_expires_at='2000-01-01' \
                  WHERE tenant_id=$1 AND run_id=$2"
            ),
            &[&TENANT, &run_id],
        )
        .await?;
    Ok(())
}

async fn seed_exhausted_run(
    client: &Client,
    run_id: &str,
    bundle_hash: &str,
    stream_seq: i64,
) -> anyhow::Result<()> {
    seed_run(client, run_id, "cat-main", bundle_hash, stream_seq).await?;
    client
        .execute(
            &format!(
                "UPDATE {SCHEMA}.run_queue \
                    SET lease_owner='dead', lease_expires_at='2000-01-01', \
                        attempts=max_attempts \
                  WHERE tenant_id=$1 AND run_id=$2"
            ),
            &[&TENANT, &run_id],
        )
        .await?;
    Ok(())
}

async fn make_callerless(client: &Client, run_id: &str) -> anyhow::Result<()> {
    client
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET trigger_source=NULL \
                  WHERE tenant_id=$1 AND run_id=$2"
            ),
            &[&TENANT, &run_id],
        )
        .await?;
    Ok(())
}

async fn install_prior_caller_winner(client: &Client, run_id: &str) -> anyhow::Result<Value> {
    client
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs \
                    SET trigger_source='http', caller_outcome_kind='responded', \
                        caller_outcome_json='{{\"winner\":\"prior\"}}', \
                        caller_http_status=207, caller_release_node_id='prior-node', \
                        caller_outcome_hash=$3, \
                        caller_released_at='2025-01-02T03:04:05.123456Z' \
                  WHERE tenant_id=$1 AND run_id=$2"
            ),
            &[&TENANT, &run_id, &PRIOR_WINNER_HASH],
        )
        .await?;
    caller_fields(client, run_id).await
}

/// The crash-evidence attempt count a run's queue row carries.
async fn queue_attempts(client: &Client, run_id: &str) -> anyhow::Result<i32> {
    Ok(client
        .query_one(
            &format!("SELECT attempts FROM {SCHEMA}.run_queue WHERE tenant_id=$1 AND run_id=$2"),
            &[&TENANT, &run_id],
        )
        .await?
        .get(0))
}

/// The claim-time `(release version, manifest digest)` a run carries.
async fn release_record(
    client: &Client,
    run_id: &str,
) -> anyhow::Result<(Option<i32>, Option<String>)> {
    let row = client
        .query_one(
            &format!(
                "SELECT release_version, manifest_digest \
                   FROM {SCHEMA}.runs WHERE tenant_id=$1 AND run_id=$2"
            ),
            &[&TENANT, &run_id],
        )
        .await?;
    Ok((row.get(0), row.get(1)))
}

async fn caller_fields(client: &Client, run_id: &str) -> anyhow::Result<Value> {
    let encoded: String = client
        .query_one(
            &format!(
                "SELECT jsonb_build_object( \
                    'kind',caller_outcome_kind, 'body',caller_outcome_json, \
                    'status',caller_http_status, 'node',caller_release_node_id, \
                    'hash',caller_outcome_hash, 'released-at',caller_released_at)::text \
                   FROM {SCHEMA}.runs WHERE tenant_id=$1 AND run_id=$2"
            ),
            &[&TENANT, &run_id],
        )
        .await?
        .get(0);
    Ok(serde_json::from_str(&encoded)?)
}

async fn assert_callerless_terminal(
    client: &Client,
    run_id: &str,
    status: &str,
) -> anyhow::Result<()> {
    assert_eq!(
        caller_fields(client, run_id).await?,
        json!({
            "kind": null,
            "body": null,
            "status": null,
            "node": null,
            "hash": null,
            "released-at": null
        })
    );
    assert_terminal_status_dequeued(client, run_id, status).await
}

async fn assert_prior_winner_terminal(
    client: &Client,
    run_id: &str,
    status: &str,
    winner: &Value,
) -> anyhow::Result<()> {
    assert_eq!(caller_fields(client, run_id).await?, *winner);
    assert_eq!(winner["kind"], "responded");
    assert_eq!(winner["body"], json!({"winner": "prior"}));
    assert_eq!(winner["status"], 207);
    assert_eq!(winner["node"], "prior-node");
    assert_eq!(winner["hash"], PRIOR_WINNER_HASH);
    assert!(winner["released-at"].as_str().is_some());
    assert_terminal_status_dequeued(client, run_id, status).await
}

async fn assert_terminal_status_dequeued(
    client: &Client,
    run_id: &str,
    status: &str,
) -> anyhow::Result<()> {
    let row = client
        .query_one(
            &format!(
                "SELECT status, NOT EXISTS ( \
                    SELECT 1 FROM {SCHEMA}.run_queue q \
                     WHERE q.tenant_id=r.tenant_id AND q.run_id=r.run_id) \
                   FROM {SCHEMA}.runs r WHERE tenant_id=$1 AND run_id=$2"
            ),
            &[&TENANT, &run_id],
        )
        .await?;
    assert_eq!(row.get::<_, String>(0), status);
    assert!(row.get::<_, bool>(1));
    Ok(())
}

fn ready_run(result: ProductionClaimResult) -> String {
    match result {
        ProductionClaimResult::Ready { run_id, .. } => run_id,
        other => panic!("expected ready claim, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a disposable PostgreSQL 18 URL in WAMN_PRODUCTION_CLAIM_PG_URL"]
async fn production_claim_live() -> anyhow::Result<()> {
    let url = std::env::var("WAMN_PRODUCTION_CLAIM_PG_URL")
        .context("set WAMN_PRODUCTION_CLAIM_PG_URL to a disposable PostgreSQL database")?;
    let admin = connect(&url).await?;
    install_schema(&admin).await?;
    let (writer, writer_role) = install_effect_writer(&admin, &url).await?;
    let runtime_url = url_with_application_name(&url, "production-claim-live-runtime")?;

    let plugin = Arc::new(WamnPostgres::new(WamnPostgresConfig {
        database_url: Some(runtime_url),
        pool_max_size: 8,
        wait_timeout_ms: 5_000,
        statement_timeout_ms: 10_000,
        row_limit: 10_000,
    })?);
    plugin.set_tenant(COMPONENT, TENANT)?;
    plugin.set_schema(COMPONENT, SCHEMA)?;
    plugin.set_runner(COMPONENT, COMPONENT)?;
    plugin.set_release_identity(COMPONENT, POD_RELEASE_VERSION, POD_MANIFEST_DIGEST)?;
    plugin.set_tenant(ROLLED_COMPONENT, TENANT)?;
    plugin.set_schema(ROLLED_COMPONENT, SCHEMA)?;
    plugin.set_runner(ROLLED_COMPONENT, ROLLED_COMPONENT)?;
    plugin.set_release_identity(
        ROLLED_COMPONENT,
        ROLLED_RELEASE_VERSION,
        ROLLED_MANIFEST_DIGEST,
    )?;

    let (release_hash, release_bytes) = plan_bytes(RELEASE_ARTIFACT);
    seed_release(&admin, "cat-main", &release_hash, &release_bytes).await?;

    // Exact global FIFO: stream sequence, then run id, breaks equal timestamps.
    for (run_id, stream_seq) in [("fifo-c", 2), ("fifo-b", 1), ("fifo-a", 1)] {
        seed_run(&admin, run_id, "cat-main", &release_hash, stream_seq).await?;
    }
    let mut fifo = Vec::new();
    for _ in 0..3 {
        fifo.push(ready_run(
            plugin.claim_next_production(COMPONENT, 30_000).await?,
        ));
    }
    assert_eq!(fifo, ["fifo-a", "fifo-b", "fifo-c"]);

    // A second claimer skips the exact FIFO head while the first claimer holds
    // its production row locks, then the rolled-back head remains claimable.
    seed_run(&admin, "double-a", "cat-main", &release_hash, 10).await?;
    seed_run(&admin, "double-b", "cat-main", &release_hash, 11).await?;
    let first_claimer = connect(&url).await?;
    first_claimer
        .batch_execute(&format!(
            "BEGIN; SET LOCAL search_path TO {SCHEMA}, pg_catalog"
        ))
        .await?;
    first_claimer
        .execute("SELECT set_config('app.tenant', $1, true)", &[&TENANT])
        .await?;
    let locked = first_claimer
        .query_one(&select_production_claim_sql(), &[])
        .await?;
    assert_eq!(locked.get::<_, String>(0), "double-a");
    let skipped = ready_run(plugin.claim_next_production(COMPONENT, 30_000).await?);
    assert_eq!(skipped, "double-b");
    first_claimer.batch_execute("ROLLBACK").await?;
    let released = ready_run(plugin.claim_next_production(COMPONENT, 30_000).await?);
    let claimed = BTreeSet::from([skipped, released]);
    assert_eq!(
        claimed,
        BTreeSet::from(["double-a".into(), "double-b".into()])
    );

    // Expired pre-effect recovery deletes only node projections and NULLs only
    // state_json; every other admitted or lineage column survives the retry.
    // `status` and the claim-time release record are excluded from the snapshot
    // because the retry CLAIM writes them; they are asserted separately below.
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs \
                   (tenant_id,run_id,flow_id,flow_version,status,catalog_id,catalog_version, \
                    environment,execution_bundle_hash,input_json,state_json,invocation_context, \
                    trigger_source,event_source_run_id,event_root_run_id,event_depth, \
                    admission_context_version,platform_revision,capture_mode,idempotency_key, \
                    response_deadline_at,run_deadline_at) \
                 VALUES ($1,'pre-effect','root',1,'running','cat-main',1,'test',$2, \
                    '{{\"input\":7}}','{{\"cursor\":9}}','{{\"source\":{{\"case\":\"a\"}}}}', \
                    'event','source-run','root-run',3,'0.1','platform-a','full','idem-a', \
                    '2030-01-01','2030-01-02')"
            ),
            &[&TENANT, &release_hash],
        )
        .await?;
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.run_queue \
                   (tenant_id,run_id,available_at,stream_seq,lease_owner,lease_expires_at, \
                    lease_generation,attempts,max_attempts) \
                 VALUES ($1,'pre-effect','2000-01-01',20,'dead','2000-01-01',4,1,3)"
            ),
            &[&TENANT],
        )
        .await?;
    admin
        .execute(
            &format!("INSERT INTO {SCHEMA}.node_runs VALUES ($1,'pre-effect','old-node')"),
            &[&TENANT],
        )
        .await?;
    let before: Value = serde_json::from_str(
        &admin
        .query_one(
            &format!(
                "SELECT (to_jsonb(r) - ARRAY['state_json','status','updated_at',\
                    'release_version','manifest_digest']::text[])::text \
                   FROM {SCHEMA}.runs AS r WHERE tenant_id=$1 AND run_id='pre-effect'"
            ),
            &[&TENANT],
        )
        .await?
        .get::<_, String>(0),
    )?;
    let reset_required = plugin.claim_next_production(COMPONENT, 30_000).await?;
    let reset_fence = match &reset_required {
        ProductionClaimResult::ResetRequired {
            run_id,
            prior_lease_owner,
            prior_lease_expires_at,
            prior_lease_generation,
        } => ResetProjectionFence {
            run_id,
            prior_lease_owner,
            prior_lease_expires_at,
            prior_lease_generation: *prior_lease_generation,
        },
        other => panic!("expected private projection reset handoff, got {other:?}"),
    };
    assert!(
        admin
            .query_one(
                &format!("SELECT state_json IS NOT NULL FROM {SCHEMA}.runs WHERE tenant_id=$1 AND run_id='pre-effect'"),
                &[&TENANT],
            )
            .await?
            .get::<_, bool>(0),
        "the app handoff committed without clearing checkpoint state"
    );
    let wrong_generation = ResetProjectionFence {
        prior_lease_generation: reset_fence.prior_lease_generation + 1,
        ..reset_fence
    };
    let mismatch = writer
        .reset_expired_pre_effect_projection(wrong_generation)
        .await
        .expect_err("a different lease generation reset projection state");
    assert_eq!(mismatch.kind(), EffectWriterErrorKind::ResetFenceLost);
    assert_eq!(
        admin
            .query_one(
                &format!("SELECT count(*) FROM {SCHEMA}.node_runs WHERE tenant_id=$1 AND run_id='pre-effect'"),
                &[&TENANT],
            )
            .await?
            .get::<_, i64>(0),
        1,
        "fence mismatch mutated the projection"
    );
    assert_eq!(
        writer
            .reset_expired_pre_effect_projection(reset_fence)
            .await
            .map_err(anyhow::Error::new)?,
        1
    );
    let pre_effect = plugin.claim_next_production(COMPONENT, 30_000).await?;
    let (run_id, payload, lease_generation) = match pre_effect {
        ProductionClaimResult::Ready {
            run_id,
            payload,
            lease_generation,
        } => (run_id, payload, lease_generation),
        other => panic!("expected pre-effect retry to execute, got {other:?}"),
    };
    assert_eq!(run_id, "pre-effect");
    assert_eq!(lease_generation, 5);
    assert_eq!(
        serde_json::from_str::<Value>(&payload)?,
        json!({
            "input": 7,
            "causation": {"run": "pre-effect", "root": "root-run", "depth": 3}
        })
    );
    let after: Value = serde_json::from_str(
        &admin
        .query_one(
            &format!(
                "SELECT (to_jsonb(r) - ARRAY['state_json','status','updated_at',\
                    'release_version','manifest_digest']::text[])::text \
                   FROM {SCHEMA}.runs AS r WHERE tenant_id=$1 AND run_id='pre-effect'"
            ),
            &[&TENANT],
        )
        .await?
        .get::<_, String>(0),
    )?;
    assert_eq!(after, before);
    assert_eq!(
        release_record(&admin, "pre-effect").await?,
        (
            Some(POD_RELEASE_VERSION),
            Some(POD_MANIFEST_DIGEST.to_string())
        ),
        "the retry claim recorded the claiming pod's release exactly once"
    );
    let (state, nodes): (Option<String>, i64) = {
        let row = admin
            .query_one(
                &format!(
                    "SELECT r.state_json::text, \
                            (SELECT count(*) FROM {SCHEMA}.node_runs n \
                              WHERE n.tenant_id=r.tenant_id AND n.run_id=r.run_id) \
                       FROM {SCHEMA}.runs r WHERE r.tenant_id=$1 AND r.run_id='pre-effect'"
                ),
                &[&TENANT],
            )
            .await?;
        (row.get(0), row.get(1))
    };
    assert_eq!((state, nodes), (None, 0));

    // A writer that fenced and validated while the lease was live may commit
    // after the lease expires. The reaper holds the row lock, waits on the same
    // tenant/run fence, then uses a fresh snapshot and must observe the attempt.
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs \
                   (tenant_id,run_id,flow_id,flow_version,status,catalog_id,catalog_version, \
                    environment,execution_bundle_hash,trigger_source) \
                 VALUES ($1,'effect-race','root',1,'running','cat-main',1,'test',$2,'http')"
            ),
            &[&TENANT, &release_hash],
        )
        .await?;
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.run_queue \
                   (tenant_id,run_id,available_at,stream_seq,lease_owner,lease_expires_at, \
                    attempts,max_attempts) \
                 VALUES ($1,'effect-race','2000-01-01',50,'runner-a','2099-01-01',2,3)"
            ),
            &[&TENANT],
        )
        .await?;
    admin
        .batch_execute(&format!(
            "CREATE FUNCTION {SCHEMA}.hold_effect_insert() RETURNS trigger \
               LANGUAGE plpgsql AS $hold$ BEGIN \
                 PERFORM pg_advisory_xact_lock({WRITER_LATCH}); RETURN NEW; \
               END $hold$; \
             CREATE TRIGGER hold_effect_insert BEFORE INSERT ON {SCHEMA}.effect_attempts \
               FOR EACH ROW EXECUTE FUNCTION {SCHEMA}.hold_effect_insert();"
        ))
        .await?;
    admin
        .query_one("SELECT pg_advisory_lock($1)", &[&WRITER_LATCH])
        .await?;
    let writer_task = {
        let writer = writer.clone();
        tokio::spawn(async move {
            writer
                .begin_attempt(effect_attempt("effect-race", "effect-node"))
                .await
        })
    };
    wait_for_advisory_wait(&admin, None, Some(&writer_role)).await?;
    admin
        .execute(
            &format!(
                "UPDATE {SCHEMA}.run_queue \
                    SET lease_expires_at='2000-01-01', attempts=max_attempts \
                  WHERE tenant_id=$1 AND run_id='effect-race'"
            ),
            &[&TENANT],
        )
        .await?;
    let reaper = {
        let plugin = Arc::clone(&plugin);
        tokio::spawn(async move { plugin.reap_one_exhausted_production(COMPONENT, 0).await })
    };
    wait_for_advisory_wait(&admin, Some("production-claim-live-runtime"), None).await?;
    let unlocked: bool = admin
        .query_one("SELECT pg_advisory_unlock($1)", &[&WRITER_LATCH])
        .await?
        .get(0);
    assert!(unlocked);
    let inserted_effect: EffectAttempt = writer_task.await??;
    assert_eq!(
        reaper.await??,
        ProductionReapResult::EffectAttempt {
            run_id: "effect-race".into()
        }
    );
    assert_eq!(
        plugin.claim_next_production(COMPONENT, 30_000).await?,
        ProductionClaimResult::Terminalized {
            run_id: "effect-race".into(),
            status: RunStatus::EffectUncertain,
            fail_kind: FailKind::EffectUncertain,
        }
    );
    let effect = admin
        .query_one(
            &format!(
                "SELECT caller_outcome_json::text,caller_http_status,caller_outcome_hash, \
                        EXISTS (SELECT 1 FROM {SCHEMA}.run_queue q \
                                 WHERE q.tenant_id=r.tenant_id AND q.run_id=r.run_id) \
                   FROM {SCHEMA}.runs r WHERE tenant_id=$1 AND run_id='effect-race'"
            ),
            &[&TENANT],
        )
        .await?;
    let effect_body = json!({"code": "effect-uncertain", "run_id": "effect-race"});
    assert_eq!(
        serde_json::from_str::<Value>(&effect.get::<_, String>(0))?,
        effect_body
    );
    assert_eq!(effect.get::<_, i32>(1), 500);
    assert_eq!(
        effect.get::<_, String>(2),
        wamn_flow::canonical_json_sha256(&effect_body)
    );
    assert!(!effect.get::<_, bool>(3));
    assert_eq!(
        writer
            .begin_attempt(effect_attempt("effect-race", "effect-node"))
            .await?,
        inserted_effect,
        "an exact immutable retry survives later terminalization"
    );
    let inactive_new = writer
        .begin_attempt(effect_attempt("effect-race", "second-effect-node"))
        .await
        .expect_err("a new coordinate cannot begin after terminalization");
    assert_eq!(inactive_new.kind(), EffectWriterErrorKind::RunNotRunnable);

    seed_live_effect_run(&admin, "effect-callerless", &release_hash, 51).await?;
    make_callerless(&admin, "effect-callerless").await?;
    writer
        .begin_attempt(effect_attempt("effect-callerless", "effect-node"))
        .await?;
    expire_effect_run(&admin, "effect-callerless").await?;
    assert_eq!(
        plugin.claim_next_production(COMPONENT, 30_000).await?,
        ProductionClaimResult::Terminalized {
            run_id: "effect-callerless".into(),
            status: RunStatus::EffectUncertain,
            fail_kind: FailKind::EffectUncertain,
        }
    );
    assert_callerless_terminal(&admin, "effect-callerless", "effect-uncertain").await?;

    seed_live_effect_run(&admin, "effect-winner", &release_hash, 52).await?;
    let effect_winner = install_prior_caller_winner(&admin, "effect-winner").await?;
    writer
        .begin_attempt(effect_attempt("effect-winner", "effect-node"))
        .await?;
    expire_effect_run(&admin, "effect-winner").await?;
    assert_eq!(
        plugin.claim_next_production(COMPONENT, 30_000).await?,
        ProductionClaimResult::Terminalized {
            run_id: "effect-winner".into(),
            status: RunStatus::EffectUncertain,
            fail_kind: FailKind::EffectUncertain,
        }
    );
    assert_prior_winner_terminal(&admin, "effect-winner", "effect-uncertain", &effect_winner)
        .await?;

    // The effect-free exhausted path computes the generic outcome and hash in
    // the host, atomically releases an attached caller, and dequeues.
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs \
                   (tenant_id,run_id,flow_id,flow_version,status,catalog_id,catalog_version, \
                    environment,execution_bundle_hash,trigger_source) \
                 VALUES ($1,'janitor','root',1,'running','cat-main',1,'test',$2,'http')"
            ),
            &[&TENANT, &release_hash],
        )
        .await?;
    admin
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.run_queue \
                   (tenant_id,run_id,available_at,stream_seq,lease_owner,lease_expires_at, \
                    attempts,max_attempts) \
                 VALUES ($1,'janitor','2000-01-01',60,'dead','2000-01-01',3,3)"
            ),
            &[&TENANT],
        )
        .await?;
    assert_eq!(
        plugin.reap_one_exhausted_production(COMPONENT, 0).await?,
        ProductionReapResult::Reaped {
            run_id: "janitor".into()
        }
    );
    let janitor = admin
        .query_one(
            &format!(
                "SELECT status,caller_outcome_json::text,caller_http_status,caller_release_node_id, \
                        caller_outcome_hash,caller_released_at IS NOT NULL, \
                        EXISTS (SELECT 1 FROM {SCHEMA}.run_queue q \
                                 WHERE q.tenant_id=r.tenant_id AND q.run_id=r.run_id) \
                   FROM {SCHEMA}.runs r WHERE tenant_id=$1 AND run_id='janitor'"
            ),
            &[&TENANT],
        )
        .await?;
    let janitor_body = json!({"error": {
        "code": "infrastructure-failure", "flow-id": "root", "flow-version": 1,
        "run-id": "janitor"
    }});
    assert_eq!(janitor.get::<_, String>(0), "infrastructure-failure");
    assert_eq!(
        serde_json::from_str::<Value>(&janitor.get::<_, String>(1))?,
        janitor_body
    );
    assert_eq!(janitor.get::<_, i32>(2), 500);
    assert_eq!(janitor.get::<_, Option<String>>(3), None);
    assert_eq!(
        janitor.get::<_, String>(4),
        wamn_flow::canonical_json_sha256(&janitor_body)
    );
    assert!(janitor.get::<_, bool>(5));
    assert!(!janitor.get::<_, bool>(6));

    seed_exhausted_run(&admin, "janitor-callerless", &release_hash, 61).await?;
    make_callerless(&admin, "janitor-callerless").await?;
    assert_eq!(
        plugin.reap_one_exhausted_production(COMPONENT, 0).await?,
        ProductionReapResult::Reaped {
            run_id: "janitor-callerless".into()
        }
    );
    assert_callerless_terminal(&admin, "janitor-callerless", "infrastructure-failure").await?;

    seed_exhausted_run(&admin, "janitor-winner", &release_hash, 62).await?;
    let janitor_winner = install_prior_caller_winner(&admin, "janitor-winner").await?;
    assert_eq!(
        plugin.reap_one_exhausted_production(COMPONENT, 0).await?,
        ProductionReapResult::Reaped {
            run_id: "janitor-winner".into()
        }
    );
    assert_prior_winner_terminal(
        &admin,
        "janitor-winner",
        "infrastructure-failure",
        &janitor_winner,
    )
    .await?;

    // ---- a refused grant must not starve its own janitor (wamn-0h0g.15.69) --
    //
    // `attempts` used to ride the same statement that grants the lease, so any
    // refusal of that statement rolled the increment back with it: the run
    // could never reach `max_attempts`, the janitor could never reap it, and
    // nothing locked it after rollback — one run head-of-line-blocked the
    // tenant forever. The advance now runs before the grant's subtransaction,
    // so a refusal still counts as crash evidence. A probe trigger stands in
    // for any database guard that can refuse the grant.
    seed_run(&admin, "grant-refused", "cat-main", &release_hash, 65).await?;
    admin
        .execute(
            &format!(
                "UPDATE {SCHEMA}.run_queue \
                    SET lease_owner='dead', lease_expires_at='2000-01-01', \
                        attempts=max_attempts-1 \
                  WHERE tenant_id=$1 AND run_id='grant-refused'"
            ),
            &[&TENANT],
        )
        .await?;
    admin
        .batch_execute(&format!(
            "CREATE FUNCTION {SCHEMA}.refuse_probed_grant() \
               RETURNS trigger LANGUAGE plpgsql AS $probe$ \
               BEGIN \
                 IF NEW.run_id = 'grant-refused' THEN \
                   RAISE EXCEPTION USING ERRCODE = '55000', \
                     MESSAGE = 'probe-grant-refused'; \
                 END IF; \
                 RETURN NEW; \
               END $probe$; \
             CREATE TRIGGER refuse_probed_grant BEFORE UPDATE OF status \
               ON {SCHEMA}.runs FOR EACH ROW \
               EXECUTE FUNCTION {SCHEMA}.refuse_probed_grant();"
        ))
        .await?;
    let starved = plugin
        .claim_next_production(COMPONENT, 30_000)
        .await
        .expect_err("the probed grant refuses");
    assert_eq!(starved.kind(), ProductionClaimErrorKind::Storage);
    assert_eq!(starved.operation(), "grant production lease");
    assert!(
        starved.to_string().contains("probe-grant-refused"),
        "unexpected refusal: {starved}"
    );
    assert_eq!(
        queue_attempts(&admin, "grant-refused").await?,
        3,
        "the refused claim rolled its own crash evidence back"
    );
    admin
        .batch_execute(&format!(
            "DROP TRIGGER refuse_probed_grant ON {SCHEMA}.runs; \
             DROP FUNCTION {SCHEMA}.refuse_probed_grant();"
        ))
        .await?;
    assert_eq!(
        plugin.reap_one_exhausted_production(COMPONENT, 0).await?,
        ProductionReapResult::Reaped {
            run_id: "grant-refused".into()
        },
        "the budget the refusal spent must reach the janitor"
    );
    assert_terminal_status_dequeued(&admin, "grant-refused", "infrastructure-failure").await?;

    // ---- the claim-time release record (wamn-0h0g.15.11, carrying the two
    // surviving proof legs of the superseded wamn-0h0g.4.14) -----------------
    //
    // MID-RUN REPUBLISH INVISIBILITY. The run is admitted under catalog version
    // 1; a republish then lands version 2 between admission and claim. The pair
    // the claim records is the CLAIMING POD's (version 7), so it matches neither
    // the admitted release nor the republished one, and the run's own admission
    // identity is untouched.
    seed_run(&admin, "release-record", "cat-main", &release_hash, 70).await?;
    assert_eq!(
        release_record(&admin, "release-record").await?,
        (None, None)
    );
    admin
        .execute(
            "INSERT INTO catalog.release_flows \
               (tenant_id,catalog_id,catalog_version,flow_id,flow_version,execution_bundle_hash) \
             VALUES ($1,'cat-main',2,'root',1,$2)",
            &[&TENANT, &release_hash],
        )
        .await?;
    assert_eq!(
        ready_run(plugin.claim_next_production(COMPONENT, 30_000).await?),
        "release-record"
    );
    let recorded = (
        Some(POD_RELEASE_VERSION),
        Some(POD_MANIFEST_DIGEST.to_string()),
    );
    assert_eq!(release_record(&admin, "release-record").await?, recorded);
    assert_eq!(
        admin
            .query_one(
                &format!(
                    "SELECT catalog_version FROM {SCHEMA}.runs \
                      WHERE tenant_id=$1 AND run_id='release-record'"
                ),
                &[&TENANT],
            )
            .await?
            .get::<_, i32>(0),
        1,
        "a republish must not move the run's admitted release either"
    );

    // SAME-RELEASE RE-CLAIM. The classifier's pre-effect reclaim clears the
    // abandoned attempt's pair and the grant records this pod's again, so the
    // observable pair is unchanged.
    expire_effect_run(&admin, "release-record").await?;
    assert_eq!(
        ready_run(plugin.claim_next_production(COMPONENT, 30_000).await?),
        "release-record"
    );
    assert_eq!(release_record(&admin, "release-record").await?, recorded);

    // RESET PER CLAIM ATTEMPT (wamn-0h0g.15.55). A pod carrying a DIFFERENT
    // release re-claims an expired pre-effect run successfully: the pair is
    // write-once per ATTEMPT, and the classifier's reset — not an exception in
    // the guard — is what lets the next claim record afresh. Under a rollout
    // this is the normal case, not an edge case.
    expire_effect_run(&admin, "release-record").await?;
    assert_eq!(
        ready_run(
            plugin
                .claim_next_production(ROLLED_COMPONENT, 30_000)
                .await?
        ),
        "release-record"
    );
    let rerecorded = (
        Some(ROLLED_RELEASE_VERSION),
        Some(ROLLED_MANIFEST_DIGEST.to_string()),
    );
    assert_eq!(
        release_record(&admin, "release-record").await?,
        rerecorded,
        "the reclaiming pod records its own release, not the dead attempt's"
    );

    // THE ERASURE IS NOT A BLANKET HOLE. The guard permits value -> NULL only
    // while nothing references the identity being erased. A run that executed,
    // fired an effect, or reached a terminal status keeps its audit link, and
    // value -> value' is refused on every path.
    admin
        .execute(
            &format!("INSERT INTO {SCHEMA}.node_runs VALUES ($1,'release-record','a-node')"),
            &[&TENANT],
        )
        .await?;
    let third_pair = format!(
        "release_version=9, manifest_digest={}",
        quote_literal(EMPTY_HASH)
    );
    for (set, why) in [
        (
            "release_version=NULL, manifest_digest=NULL".to_string(),
            "a run with a node projection may not erase its release record",
        ),
        (
            third_pair,
            "no path may rewrite a recorded release in place",
        ),
    ] {
        let refused = admin
            .execute(
                &format!(
                    "UPDATE {SCHEMA}.runs SET {set} \
                      WHERE tenant_id=$1 AND run_id='release-record'"
                ),
                &[&TENANT],
            )
            .await
            .expect_err(why);
        let db = refused.as_db_error().expect("guard refusal is a db error");
        assert_eq!(db.code().code(), "55000");
        assert_eq!(db.message(), "run-release-record-immutable", "{why}");
    }
    admin
        .execute(
            &format!(
                "DELETE FROM {SCHEMA}.node_runs \
                  WHERE tenant_id=$1 AND run_id='release-record'"
            ),
            &[&TENANT],
        )
        .await?;
    admin
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET status='completed' \
                  WHERE tenant_id=$1 AND run_id='release-record'"
            ),
            &[&TENANT],
        )
        .await?;
    let terminal = admin
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET release_version=NULL, manifest_digest=NULL \
                  WHERE tenant_id=$1 AND run_id='release-record'"
            ),
            &[&TENANT],
        )
        .await
        .expect_err("a terminal run keeps the audit link to the plan hashes it ran");
    assert_eq!(
        terminal
            .as_db_error()
            .expect("guard refusal is a db error")
            .message(),
        "run-release-record-immutable"
    );
    assert_eq!(release_record(&admin, "release-record").await?, rerecorded);
    admin
        .execute(
            &format!(
                "DELETE FROM {SCHEMA}.run_queue \
                  WHERE tenant_id=$1 AND run_id='release-record'"
            ),
            &[&TENANT],
        )
        .await?;

    // PARK/WAKE IS NOT COVERED BY THE CLASSIFIER RESET. `park_sql` releases the
    // lease (`lease_owner`/`lease_expires_at` to NULL), so a doorbell wake
    // classifies as `Ordinary` and goes straight to the grant with no reset in
    // between. A waking pod carrying a different release therefore still hits
    // the guard, and because a released lease is not crash evidence the refusal
    // spends no budget — so the janitor cannot retire the run either. This leg
    // pins the gap the wamn-0h0g.15.55 ruling assumed was already closed.
    seed_run(&admin, "park-wake", "cat-main", &release_hash, 71).await?;
    assert_eq!(
        ready_run(plugin.claim_next_production(COMPONENT, 30_000).await?),
        "park-wake"
    );
    assert_eq!(release_record(&admin, "park-wake").await?, recorded);
    admin
        .execute(
            &format!(
                "UPDATE {SCHEMA}.run_queue \
                    SET available_at=now(), lease_owner=NULL, lease_expires_at=NULL \
                  WHERE tenant_id=$1 AND run_id='park-wake'"
            ),
            &[&TENANT],
        )
        .await?;
    let woken = plugin
        .claim_next_production(ROLLED_COMPONENT, 30_000)
        .await
        .expect_err("a rolled pod waking a parked run has no classifier reset");
    assert_eq!(woken.kind(), ProductionClaimErrorKind::Storage);
    assert_eq!(woken.operation(), "grant production lease");
    assert!(
        woken.to_string().contains("run-release-record-immutable"),
        "unexpected refusal: {woken}"
    );
    assert_eq!(
        release_record(&admin, "park-wake").await?,
        recorded,
        "the refused wake left the recorded pair exactly as it was"
    );
    assert_eq!(
        queue_attempts(&admin, "park-wake").await?,
        0,
        "a released lease is not crash evidence, so this refusal spends no budget"
    );
    admin
        .execute(
            &format!("DELETE FROM {SCHEMA}.run_queue WHERE tenant_id=$1 AND run_id='park-wake'"),
            &[&TENANT],
        )
        .await?;

    drop(plugin);
    drop(writer);
    let writer_role = quote_identifier(&writer_role);
    admin
        .batch_execute(&format!(
            "DROP SCHEMA {SCHEMA} CASCADE; DROP SCHEMA catalog CASCADE; \
             DO $disconnect$ BEGIN EXECUTE format( \
               'REVOKE CONNECT ON DATABASE %I FROM {writer_role}', current_database()); \
             END $disconnect$; \
             REVOKE wamn_effect_writer, wamn_run_projection_writer FROM {writer_role}; \
             ALTER ROLE {writer_role} NOLOGIN PASSWORD NULL VALID UNTIL 'epoch';"
        ))
        .await?;
    Ok(())
}
