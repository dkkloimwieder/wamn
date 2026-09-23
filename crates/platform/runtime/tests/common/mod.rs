//! Fixture scaffolding shared by the two production-claim live suites.
//!
//! wamn-0h0g.20.4 split the one live claim test into a SURVIVING SPINE
//! (`production_claim_live.rs`, the default `standard` class) and a SHELVED
//! FLOOR (`production_claim_durable_live.rs`, the premium `durable` class).
//! Both build the identical fixture, so the fixture lives here exactly once and
//! neither suite can drift into showing a different schema than the other.
//!
//! Rust compiles this module separately into each test binary, so items only
//! one suite uses are dead in the other; `dead_code` is allowed for that reason
//! and no other.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio_postgres::{Client, GenericClient, NoTls};
use url::Url;
use wamn_run_state::AuthorityClass;
use wamn_runtime::plugins::wamn_postgres::{
    ClassCredentials, ProductionClaimResult, WamnPostgres, WamnPostgresConfig,
};

pub const TENANT: &str = "claim-live";
pub const COMPONENT: &str = "claim-live-runner";
pub const PACKAGE_ID: &str = "cat_main";
pub const ENVIRONMENT: &str = "test";
/// A second pod carrying a different effective release — the mismatch case.
pub const ROLLED_COMPONENT: &str = "claim-live-runner-next";
pub const SCHEMA: &str = "wamn_run";
/// The exact effective release pinned on every ordinarily seeded run.
pub const POD_EFFECTIVE_RELEASE_ID: i32 = 1;
pub const POD_MANIFEST_DIGEST: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";
pub const ROLLED_EFFECTIVE_RELEASE_ID: i32 = 2;
pub const ROLLED_MANIFEST_DIGEST: &str =
    "sha256:2222222222222222222222222222222222222222222222222222222222222222";
pub const WIRING_ID: &str = "claim-live-wiring";
pub const WIRING_VERSION: i32 = 1;
pub const EMPTY_HASH: &str =
    "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";
const EXECUTOR_PASSWORD: &str = "claim-live-executor-password";
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

pub fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Record one attributed effect attempt for `run_id`, as the fixture superuser.
pub async fn insert_effect_attempt(
    client: &impl GenericClient,
    run_id: &str,
    local_node_id: &str,
) -> anyhow::Result<()> {
    client
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.effect_attempts \
                   (tenant_id,run_id,root_plan_hash,current_plan_hash,frame_id, \
                    local_node_id,source_artifact_hash,requirement_name,occurrence,seq, \
                    generation_fact_kind,attempt_deadline_at,attempt_input_ref) \
                 VALUES ($1,$2,$3,$3,0,$4,$3,'manager',0,1, \
                         'not-required','2099-01-01T00:00:00Z','sha256:claim-live-effect-input')"
            ),
            &[&TENANT, &run_id, &EMPTY_HASH, &local_node_id],
        )
        .await?;
    Ok(())
}

const RUN_STATE_SQL: &str = include_str!("../../../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../../../deploy/sql/run-queue.sql");

/// Apply the run plane of record on the tenant floor, with the two effective
/// releases the pods mount.
pub async fn install_schema(client: &Client) -> anyhow::Result<()> {
    client.batch_execute(RUN_STATE_SQL).await?;
    client.batch_execute(RUN_QUEUE_SQL).await?;
    client
        .batch_execute(&format!(
            "INSERT INTO catalog.effective_releases (tenant_id, effective_release_id, environment) \
             VALUES ('{TENANT}', {POD_EFFECTIVE_RELEASE_ID}, '{ENVIRONMENT}'), \
                    ('{TENANT}', {ROLLED_EFFECTIVE_RELEASE_ID}, '{ENVIRONMENT}');"
        ))
        .await?;
    Ok(())
}

/// Mint the executor generation separately from the fixture's administrator.
async fn install_executor(client: &Client, admin_url: &str) -> anyhow::Result<(String, String)> {
    use wamn_control_provision::sql::prepare_workload_generation_sql;
    use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleScope, workload_generation_role};

    let database: String = client
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    let family = WorkloadRoleFamily::ExecutorPlatform;
    let role = workload_generation_role(
        family,
        WorkloadRoleScope::ProjectEnvironment {
            org: "claim-live-org",
            project: "claim-live-project",
            environment: ENVIRONMENT,
            database: &database,
        },
        wamn_control_provision::CredentialGeneration::A,
    )?;
    // The production builder: the stable executor surface, the `wamn_platform`
    // group edge, the generation login and its `CONNECT`.
    client
        .batch_execute(&prepare_workload_generation_sql(
            family,
            &database,
            &role,
            EXECUTOR_PASSWORD,
            "2099-01-01T00:00:00Z",
        ))
        .await?;
    let mut url = Url::parse(admin_url)?;
    url.set_username(&role)
        .map_err(|()| anyhow::anyhow!("set executor username"))?;
    url.set_password(Some(EXECUTOR_PASSWORD))
        .map_err(|()| anyhow::anyhow!("set executor password"))?;
    url.set_fragment(None);
    Ok((
        url_with_application_name(url.as_str(), RUNTIME_APPLICATION_NAME)?,
        role,
    ))
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
    package_id: &str,
    stream_seq: i64,
) -> anyhow::Result<()> {
    seed_run_of_class(client, run_id, package_id, stream_seq, "standard").await
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
    package_id: &str,
    stream_seq: i64,
) -> anyhow::Result<()> {
    seed_run_of_class(client, run_id, package_id, stream_seq, "durable").await
}

async fn seed_run_of_class(
    client: &Client,
    run_id: &str,
    package_id: &str,
    stream_seq: i64,
    durability_class: &str,
) -> anyhow::Result<()> {
    client
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs \
                   (tenant_id,run_id,flow_id,flow_version,status,package_id,effective_release_id, \
                    environment,wiring_id,wiring_version,input_json,trigger_source, \
                    durability_class) \
                 VALUES ($1,$2,'root',1,'dispatched',$3,1,'test',$4,$5, \
                         '{{\"input\":true}}','http',$6)"
            ),
            &[
                &TENANT,
                &run_id,
                &package_id,
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
/// effect-uncertain test seeded `standard` would not exercise that class. Saying
/// `durable` here is what keeps these legs pointed at the tier they belong to.
pub async fn seed_live_effect_run(
    client: &Client,
    run_id: &str,
    stream_seq: i64,
) -> anyhow::Result<()> {
    seed_durable_run(client, run_id, PACKAGE_ID, stream_seq).await?;
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
    seed_run(client, run_id, PACKAGE_ID, stream_seq).await?;
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

/// The admission-pinned effective release and claim-time manifest digest a run carries.
pub async fn release_record(
    client: &Client,
    run_id: &str,
) -> anyhow::Result<(i32, Option<String>)> {
    let row = client
        .query_one(
            &format!(
                "SELECT effective_release_id, manifest_digest \
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
    executor_role: String,
}

/// Install the schema, the executor credential, and pod identities.
///
/// Both suites call this with the same setup. Testing the queue
/// against a different schema than the shelved floor would show nothing about
/// the floor's removal.
pub async fn install_fixture(url: &str) -> anyhow::Result<LiveFixture> {
    let admin = connect(url).await?;
    install_schema(&admin).await?;
    let (runtime_url, executor_role) = install_executor(&admin, url).await?;

    let plugin = Arc::new(WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(
            ClassCredentials::default().with_class(AuthorityClass::ExecutorPlatform, runtime_url),
        ),
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
        POD_EFFECTIVE_RELEASE_ID,
        wamn_catalog::ManifestDigest::parse(POD_MANIFEST_DIGEST)?,
    )?;
    plugin.set_tenant(ROLLED_COMPONENT, TENANT)?;
    plugin.set_schema(ROLLED_COMPONENT, SCHEMA)?;
    plugin.set_runner(ROLLED_COMPONENT, ROLLED_COMPONENT)?;
    plugin.set_release_identity(
        ROLLED_COMPONENT,
        ROLLED_EFFECTIVE_RELEASE_ID,
        wamn_catalog::ManifestDigest::parse(ROLLED_MANIFEST_DIGEST)?,
    )?;

    Ok(LiveFixture {
        admin,
        plugin,
        executor_role,
    })
}

/// Drop the fixture schemas and retire the executor generation role the suite minted.
pub async fn teardown(fixture: LiveFixture) -> anyhow::Result<()> {
    let LiveFixture {
        admin,
        plugin,
        executor_role,
    } = fixture;
    drop(plugin);
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    admin
        .batch_execute(
            &wamn_control_provision::sql::retire_workload_generation_sql(
                wamn_control_provision::WorkloadRoleFamily::ExecutorPlatform,
                &database,
                &executor_role,
            ),
        )
        .await?;
    admin
        .batch_execute(
            &wamn_control_provision::sql::terminate_workload_generation_sessions_sql(
                &executor_role,
            ),
        )
        .await?;
    admin
        .batch_execute(&format!(
            "DROP SCHEMA {SCHEMA} CASCADE; DROP SCHEMA catalog CASCADE;"
        ))
        .await?;
    Ok(())
}
