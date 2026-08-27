//! Fixture scaffolding shared by the two production-claim live suites.
//!
//! wamn-0h0g.20.4 split the one live claim proof into a SURVIVING SPINE
//! (`production_claim_live.rs`, the default `standard` class) and a SHELVED
//! FLOOR (`production_claim_durable_live.rs`, the premium `durable` class).
//! Both build the identical fixture, so the fixture lives here exactly once and
//! neither suite can drift into proving a different schema than the other.
//!
//! Rust compiles this module separately into each test binary, so items only
//! one suite uses are dead in the other; `dead_code` is allowed for that reason
//! and no other.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};
use url::Url;
use wamn_run_state::{
    BeginEffectAttempt, CredentialGeneration, EffectWriterClient, EffectWriterCredentialScope,
    EffectWriterCredentialValidity, EffectWriterScope, effect_writer_credential,
    effect_writer_generation_role,
};
use wamn_runtime::plugins::wamn_postgres::{
    ClassCredentials, ProductionClaimResult, WamnPostgres, WamnPostgresConfig,
};

pub const TENANT: &str = "claim-live";
pub const COMPONENT: &str = "claim-live-runner";
pub const CATALOG_ID: &str = "cat-main";
pub const ENVIRONMENT: &str = "test";
/// A second pod, carrying a DIFFERENT release — the rollout case.
pub const ROLLED_COMPONENT: &str = "claim-live-runner-next";
pub const SCHEMA: &str = "wamn_claim_live";
/// The release the claiming pod carries. Deliberately distinct from every
/// admitted catalog version in this fixture, so a record copied from the run
/// instead of the claiming pod would be visible.
pub const POD_RELEASE_VERSION: i32 = 7;
pub const POD_MANIFEST_DIGEST: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";
pub const ROLLED_RELEASE_VERSION: i32 = 8;
pub const ROLLED_MANIFEST_DIGEST: &str =
    "sha256:2222222222222222222222222222222222222222222222222222222222222222";
pub const WIRING_ID: &str = "claim-live-wiring";
pub const WIRING_VERSION: i32 = 1;
pub const EMPTY_HASH: &str =
    "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";
pub const WRITER_PASSWORD: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
pub const WRITER_LATCH: i64 = 7_141_013;
pub const PRIOR_WINNER_HASH: &str = "sha256:prior-caller-winner";

pub async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("production-claim live connection failed: {error}");
        }
    });
    Ok(client)
}

pub fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

pub fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub fn effect_attempt(
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

pub fn run_state_stand_in_ddl() -> String {
    format!(
        "CREATE TABLE {SCHEMA}.runs ( \
           tenant_id text NOT NULL, run_id text NOT NULL, flow_id text NOT NULL, \
           flow_version int NOT NULL, catalog_id text NOT NULL, catalog_version int NOT NULL, \
           environment text NOT NULL, \
           attachment_id text, registration_id text, \
           event_source_run_id text, event_root_run_id text, event_depth int, \
           status text NOT NULL \
             CHECK (status IN ('dispatched', 'running', 'completed', 'failed', \
                               'infrastructure-failure', 'effect-uncertain')), \
           trigger_source text, capture_mode text, \
           durability_class text NOT NULL DEFAULT 'standard' \
             CHECK (durability_class IN ('standard', 'durable')), \
           wiring_id text, wiring_version int, \
           wiring_hash text, binding_world_json jsonb, \
           release_version int, manifest_digest text, \
           input_json jsonb NOT NULL DEFAULT '{{}}', result_json jsonb, state_json jsonb, \
           invocation_context jsonb NOT NULL DEFAULT '{{}}', \
           admission_context_version text, platform_revision text, idempotency_key text, \
           caller_outcome_kind text, caller_outcome_json jsonb, caller_http_status int, \
           caller_release_node_id text, caller_outcome_hash text, \
           caller_released_at timestamptz, response_deadline_at timestamptz, \
           run_deadline_at timestamptz, terminal_reason text, \
           fail_kind text, \
           created_at timestamptz NOT NULL DEFAULT now(), \
           updated_at timestamptz NOT NULL DEFAULT now(), \
           CONSTRAINT runs_release_record_check CHECK ( \
             (release_version IS NULL AND manifest_digest IS NULL) \
             OR (release_version IS NOT NULL AND manifest_digest IS NOT NULL \
                 AND release_version > 0 \
                 AND manifest_digest ~ '^sha256:[0-9a-f]{{64}}$')), \
           CONSTRAINT runs_wiring_identity_check CHECK ( \
             (wiring_id IS NULL AND wiring_version IS NULL) \
             OR (wiring_id IS NOT NULL AND wiring_version IS NOT NULL \
                 AND wiring_id <> '' AND wiring_version > 0)), \
           PRIMARY KEY (tenant_id, run_id)); \
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
           UNIQUE (tenant_id,run_id,frame_id,local_node_id,occurrence));"
    )
}

pub async fn install_schema(client: &Client) -> anyhow::Result<()> {
    let run_state_stand_in = run_state_stand_in_ddl();
    client
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             CREATE SCHEMA {SCHEMA}; \
             CREATE SCHEMA catalog; \
             {run_state_stand_in} \
             CREATE FUNCTION {SCHEMA}.guard_run_admission_pins_immutable() \
               RETURNS trigger LANGUAGE plpgsql AS $guard$ \
               BEGIN \
                 IF NEW.catalog_id IS DISTINCT FROM OLD.catalog_id \
                    OR NEW.catalog_version IS DISTINCT FROM OLD.catalog_version \
                    OR NEW.environment IS DISTINCT FROM OLD.environment \
                    OR NEW.capture_mode IS DISTINCT FROM OLD.capture_mode \
                    OR NEW.durability_class IS DISTINCT FROM OLD.durability_class \
                    OR NEW.wiring_id IS DISTINCT FROM OLD.wiring_id \
                    OR NEW.wiring_version IS DISTINCT FROM OLD.wiring_version THEN \
                   RAISE EXCEPTION USING ERRCODE = '55000', \
                     MESSAGE = 'run-admission-pin-immutable'; \
                 END IF; \
                 IF OLD.release_version IS NOT NULL \
                    OR OLD.manifest_digest IS NOT NULL THEN \
                   IF NEW.release_version IS NULL AND NEW.manifest_digest IS NULL THEN \
                     IF NEW.status NOT IN ('dispatched', 'running') \
                        OR EXISTS (SELECT 1 FROM {SCHEMA}.effect_attempts AS effect \
                                    WHERE effect.tenant_id = OLD.tenant_id \
                                      AND effect.run_id = OLD.run_id \
                                      AND OLD.durability_class = 'durable') THEN \
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
                                capture_mode, durability_class, wiring_id, wiring_version, \
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

pub async fn install_effect_writer(
    client: &Client,
    admin_url: &str,
) -> anyhow::Result<(EffectWriterClient, String)> {
    let database: String = client
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    let scope = EffectWriterCredentialScope {
        tenant: "claim-live-tenant".to_string(),
        org: "claim-live-org".to_string(),
        project: "claim-live-project".to_string(),
        environment: "claim-live-env".to_string(),
        database: database.clone(),
    };
    let role =
        effect_writer_generation_role(&scope.tenant, &scope.database, CredentialGeneration::A);
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
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname={role_literal}) THEN \
                 CREATE ROLE {role_identifier} LOGIN PASSWORD {password_literal} \
                   NOSUPERUSER NOCREATEDB NOCREATEROLE INHERIT NOREPLICATION NOBYPASSRLS; \
               ELSE \
                 ALTER ROLE {role_identifier} LOGIN PASSWORD {password_literal} \
                   NOSUPERUSER NOCREATEDB NOCREATEROLE INHERIT NOREPLICATION NOBYPASSRLS; \
               END IF; \
             END $roles$; \
             GRANT wamn_effect_writer TO {role_identifier}; \
             GRANT CONNECT ON DATABASE {} TO {role_identifier}; \
             GRANT USAGE ON SCHEMA {SCHEMA} TO wamn_effect_writer; \
             GRANT SELECT (tenant_id,run_id,status) \
               ON {SCHEMA}.runs TO wamn_effect_writer; \
             GRANT SELECT (tenant_id,run_id,lease_owner,lease_expires_at,lease_generation) \
               ON {SCHEMA}.run_queue TO wamn_effect_writer; \
             GRANT SELECT,INSERT ON {SCHEMA}.effect_attempts TO wamn_effect_writer; \
             ALTER TABLE {SCHEMA}.runs ENABLE ROW LEVEL SECURITY; \
             ALTER TABLE {SCHEMA}.runs FORCE ROW LEVEL SECURITY; \
             CREATE POLICY runs_tenant ON {SCHEMA}.runs \
               USING (tenant_id=NULLIF(current_setting('app.tenant',true),'')); \
             ALTER TABLE {SCHEMA}.run_queue ENABLE ROW LEVEL SECURITY; \
             ALTER TABLE {SCHEMA}.run_queue FORCE ROW LEVEL SECURITY; \
             CREATE POLICY run_queue_tenant ON {SCHEMA}.run_queue \
               USING (tenant_id=NULLIF(current_setting('app.tenant',true),'')); \
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

pub fn url_with_application_name(url: &str, name: &str) -> anyhow::Result<String> {
    let mut parsed = Url::parse(url)?;
    parsed
        .query_pairs_mut()
        .append_pair("application_name", name);
    Ok(parsed.into())
}

pub async fn wait_for_advisory_wait(
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

/// Seed an admitted, queued run on the DEFAULT `standard` class.
pub async fn seed_run(
    client: &Client,
    run_id: &str,
    catalog_id: &str,
    stream_seq: i64,
) -> anyhow::Result<()> {
    seed_run_of_class(client, run_id, catalog_id, stream_seq, "standard").await
}

/// Seed an admitted, queued run on the PREMIUM `durable` class.
///
/// The class is written by the ADMITTING INSERT, never by a later UPDATE: it is
/// an admission pin, and `wamn_run.guard_run_admission_pins_immutable` names
/// `durability_class` in both its trigger column list and its pin arm
/// (`deploy/sql/run-state.sql`), so promoting a run after admission is a
/// `run-admission-pin-immutable` refusal in production.
pub async fn seed_durable_run(
    client: &Client,
    run_id: &str,
    catalog_id: &str,
    stream_seq: i64,
) -> anyhow::Result<()> {
    seed_run_of_class(client, run_id, catalog_id, stream_seq, "durable").await
}

async fn seed_run_of_class(
    client: &Client,
    run_id: &str,
    catalog_id: &str,
    stream_seq: i64,
    durability_class: &str,
) -> anyhow::Result<()> {
    client
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs \
                   (tenant_id,run_id,flow_id,flow_version,status,catalog_id,catalog_version, \
                    environment,wiring_id,wiring_version,input_json,trigger_source, \
                    durability_class) \
                 VALUES ($1,$2,'root',1,'dispatched',$3,1,'test',$4,$5, \
                         '{{\"input\":true}}','http',$6)"
            ),
            &[
                &TENANT,
                &run_id,
                &catalog_id,
                &WIRING_ID,
                &WIRING_VERSION,
                &durability_class,
            ],
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

/// Seed a live-leased run ON THE PREMIUM TIER.
///
/// Every caller of this helper goes on to write an effect attempt, and the
/// crash floor that reads those attempts is class-gated (wamn-0h0g.20.2): on
/// the default `standard` class the claim takes no advisory fence, reads no
/// effect snapshot, and never classifies `ExpiredWithAttempt`, so an
/// effect-uncertain proof seeded `standard` would prove nothing. Saying
/// `durable` here is what keeps these legs pointed at the tier they belong to.
pub async fn seed_live_effect_run(
    client: &Client,
    run_id: &str,
    stream_seq: i64,
) -> anyhow::Result<()> {
    seed_durable_run(client, run_id, "cat-main", stream_seq).await?;
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

pub async fn expire_effect_run(client: &Client, run_id: &str) -> anyhow::Result<()> {
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

pub async fn seed_exhausted_run(
    client: &Client,
    run_id: &str,
    stream_seq: i64,
) -> anyhow::Result<()> {
    seed_run(client, run_id, "cat-main", stream_seq).await?;
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

pub async fn make_callerless(client: &Client, run_id: &str) -> anyhow::Result<()> {
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

pub async fn install_prior_caller_winner(client: &Client, run_id: &str) -> anyhow::Result<Value> {
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
pub async fn queue_attempts(client: &Client, run_id: &str) -> anyhow::Result<i32> {
    Ok(client
        .query_one(
            &format!("SELECT attempts FROM {SCHEMA}.run_queue WHERE tenant_id=$1 AND run_id=$2"),
            &[&TENANT, &run_id],
        )
        .await?
        .get(0))
}

/// The claim-time `(release version, manifest digest)` a run carries.
pub async fn release_record(
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

pub async fn caller_fields(client: &Client, run_id: &str) -> anyhow::Result<Value> {
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

pub async fn assert_callerless_terminal(
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

pub async fn assert_prior_winner_terminal(
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

pub async fn assert_terminal_status_dequeued(
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

pub fn ready_run(result: ProductionClaimResult) -> String {
    match result {
        ProductionClaimResult::Ready { run_id, .. } => run_id,
        other => panic!("expected ready claim, got {other:?}"),
    }
}

/// The `application_name` the runtime pool carries.
///
/// `wait_for_advisory_wait` selects the blocked reaper backend by this exact
/// string, so it is fixture-wide rather than per-suite.
pub const RUNTIME_APPLICATION_NAME: &str = "production-claim-live-runtime";

/// Everything a live claim suite needs, built once per suite.
pub struct LiveFixture {
    pub admin: Client,
    pub plugin: Arc<WamnPostgres>,
    pub writer: EffectWriterClient,
    pub writer_role: String,
}

/// Install the schema, the private effect writer, and the pod identities.
///
/// Both suites call this and neither may vary it: a spine that proved the queue
/// against a different schema than the shelved floor would prove nothing about
/// the floor's removal.
pub async fn install_fixture(url: &str) -> anyhow::Result<LiveFixture> {
    let admin = connect(url).await?;
    install_schema(&admin).await?;
    let (writer, writer_role) = install_effect_writer(&admin, url).await?;
    let runtime_url = url_with_application_name(url, RUNTIME_APPLICATION_NAME)?;

    let plugin = Arc::new(WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(runtime_url)),
        guest_pool_max_size: 8,
        platform_pool_max_size: 8,
        wait_timeout_ms: 5_000,
        statement_timeout_ms: 10_000,
        row_limit: 10_000,
    })?);
    plugin.set_tenant(COMPONENT, TENANT)?;
    plugin.set_schema(COMPONENT, SCHEMA)?;
    plugin.set_runner(COMPONENT, COMPONENT)?;
    plugin.set_release_identity(
        COMPONENT,
        POD_RELEASE_VERSION,
        wamn_catalog::ManifestDigest::parse(POD_MANIFEST_DIGEST)?,
    )?;
    plugin.set_tenant(ROLLED_COMPONENT, TENANT)?;
    plugin.set_schema(ROLLED_COMPONENT, SCHEMA)?;
    plugin.set_runner(ROLLED_COMPONENT, ROLLED_COMPONENT)?;
    plugin.set_release_identity(
        ROLLED_COMPONENT,
        ROLLED_RELEASE_VERSION,
        wamn_catalog::ManifestDigest::parse(ROLLED_MANIFEST_DIGEST)?,
    )?;

    Ok(LiveFixture {
        admin,
        plugin,
        writer,
        writer_role,
    })
}

/// Drop the fixture schemas and defuse the generation role the suite minted.
pub async fn teardown(fixture: LiveFixture) -> anyhow::Result<()> {
    let LiveFixture {
        admin,
        plugin,
        writer,
        writer_role,
        ..
    } = fixture;
    drop(plugin);
    drop(writer);
    let writer_role = quote_identifier(&writer_role);
    admin
        .batch_execute(&format!(
            "DROP SCHEMA {SCHEMA} CASCADE; DROP SCHEMA catalog CASCADE; \
             DO $disconnect$ BEGIN EXECUTE format( \
               'REVOKE CONNECT ON DATABASE %I FROM {writer_role}', current_database()); \
             END $disconnect$; \
             REVOKE wamn_effect_writer FROM {writer_role}; \
             ALTER ROLE {writer_role} NOLOGIN PASSWORD NULL VALID UNTIL 'epoch';"
        ))
        .await?;
    Ok(())
}
