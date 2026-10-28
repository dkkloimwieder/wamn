//! Shared fixtures for production queue tests.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};
use url::Url;
use wamn_run_state::AuthorityClass;
use wamn_runtime::plugins::wamn_postgres::{
    ClassCredentials, ProductionClaimResult, ReleaseIdentity, WamnPostgres, WamnPostgresConfig,
};

pub const ENV_KEY: &str = "claim-live";
pub const COMPONENT: &str = "claim-live-runner";
pub const PACKAGE_ID: &str = "cat_main";
pub const ENVIRONMENT: &str = "test";
/// A second pod carrying a different release — the mismatch case.
pub const ROLLED_COMPONENT: &str = "claim-live-runner-next";
pub const SCHEMA: &str = "wamn_run";
/// The release pinned on every ordinarily seeded run: the digest of
/// [`POD_MANIFEST_BYTES`].
pub const POD_MANIFEST_DIGEST: &str =
    "sha256:21621f6bd9bc2769154aa2938726e891ecbc7929cd7be941b60ec776703ed394";
pub const POD_MANIFEST_BYTES: &str = r#"{"release":1}"#;
/// The rolled pod's release: the digest of [`ROLLED_MANIFEST_BYTES`].
pub const ROLLED_MANIFEST_DIGEST: &str =
    "sha256:89437c2ddcb5d585633413604661041fe6b5ee9d49ae96153fe24812f1a8c5c0";
pub const ROLLED_MANIFEST_BYTES: &str = r#"{"release":2}"#;
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
    let actor = wamn_project_state::PlatformComponent::Provisioning;
    client.execute("SELECT set_config('app.user_id', $1, false), set_config('app.operation', 'admin:claim-fixture', false)", &[&actor.principal_id().to_string()]).await?;
    Ok(client)
}

pub fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

const RUN_STATE_SQL: &str = include_str!("../../../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../../../deploy/sql/run-queue.sql");

/// Apply the run plane of record on the tenant floor, with the two releases
/// the pods mount.
pub async fn install_schema(client: &Client) -> anyhow::Result<()> {
    client.batch_execute(RUN_STATE_SQL).await?;
    client.batch_execute(RUN_QUEUE_SQL).await?;
    client
        .batch_execute(wamn_control_provision::APP_SCHEMA_SQL)
        .await?;
    client
        .batch_execute(&format!(
            "INSERT INTO catalog.releases (env_key, manifest_digest, canonical_bytes) \
             VALUES ('{ENV_KEY}', '{POD_MANIFEST_DIGEST}', '{POD_MANIFEST_BYTES}'), \
                    ('{ENV_KEY}', '{ROLLED_MANIFEST_DIGEST}', '{ROLLED_MANIFEST_BYTES}');"
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

/// Seed an admitted, queued run on the `standard` class.
pub async fn seed_run(
    client: &Client,
    run_id: &str,
    package_id: &str,
    stream_seq: i64,
) -> anyhow::Result<()> {
    client
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.runs \
                   (env_key,run_id,flow_id,flow_version,status,package_id,manifest_digest, \
                    environment,wiring_id,wiring_version,input_json,trigger_source, \
                    durability_class) \
                 VALUES ($1,$2,'root',1,'dispatched',$3,'{POD_MANIFEST_DIGEST}','test',$4,$5, \
                         '{{\"input\":true}}','http','standard')"
            ),
            &[&ENV_KEY, &run_id, &package_id, &WIRING_ID, &WIRING_VERSION],
        )
        .await?;
    client
        .execute(
            &format!(
                "INSERT INTO {SCHEMA}.run_queue \
                   (env_key,run_id,available_at,stream_seq) \
                 VALUES ($1,$2,'2000-01-01 00:00:00+00',$3)"
            ),
            &[&ENV_KEY, &run_id, &stream_seq],
        )
        .await?;
    Ok(())
}

pub async fn expire_effect_run(client: &Client, run_id: &str) -> anyhow::Result<()> {
    client
        .execute(
            &format!(
                "UPDATE {SCHEMA}.run_queue SET lease_expires_at='2000-01-01' \
                  WHERE env_key=$1 AND run_id=$2"
            ),
            &[&ENV_KEY, &run_id],
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
                  WHERE env_key=$1 AND run_id=$2"
            ),
            &[&ENV_KEY, &run_id],
        )
        .await?;
    Ok(())
}

pub async fn make_callerless(client: &Client, run_id: &str) -> anyhow::Result<()> {
    client
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs SET trigger_source=NULL \
                  WHERE env_key=$1 AND run_id=$2"
            ),
            &[&ENV_KEY, &run_id],
        )
        .await?;
    Ok(())
}

pub async fn install_prior_caller_winner(client: &Client, run_id: &str) -> anyhow::Result<Value> {
    client
        .execute(
            &format!(
                "UPDATE {SCHEMA}.runs \
                    SET trigger_source='http', caller_outcome_type='responded', \
                        caller_outcome_json='{{\"winner\":\"prior\"}}', \
                        caller_http_status=207, caller_release_node_id='prior-node', \
                        caller_outcome_hash=$3, \
                        caller_released_at='2025-01-02T03:04:05.123456Z' \
                  WHERE env_key=$1 AND run_id=$2"
            ),
            &[&ENV_KEY, &run_id, &PRIOR_WINNER_HASH],
        )
        .await?;
    caller_fields(client, run_id).await
}

/// The crash-evidence attempt count a run's queue row carries.
pub async fn queue_attempts(client: &Client, run_id: &str) -> anyhow::Result<i32> {
    Ok(client
        .query_one(
            &format!("SELECT attempts FROM {SCHEMA}.run_queue WHERE env_key=$1 AND run_id=$2"),
            &[&ENV_KEY, &run_id],
        )
        .await?
        .get(0))
}

/// The admission-pinned release digest a run carries.
pub async fn release_record(client: &Client, run_id: &str) -> anyhow::Result<Option<String>> {
    let row = client
        .query_one(
            &format!(
                "SELECT manifest_digest \
                   FROM {SCHEMA}.runs WHERE env_key=$1 AND run_id=$2"
            ),
            &[&ENV_KEY, &run_id],
        )
        .await?;
    Ok(row.get(0))
}

pub async fn caller_fields(client: &Client, run_id: &str) -> anyhow::Result<Value> {
    let encoded: String = client
        .query_one(
            &format!(
                "SELECT jsonb_build_object( \
                    'kind',caller_outcome_type, 'body',caller_outcome_json, \
                    'status',caller_http_status, 'node',caller_release_node_id, \
                    'hash',caller_outcome_hash, 'released-at',caller_released_at)::text \
                   FROM {SCHEMA}.runs WHERE env_key=$1 AND run_id=$2"
            ),
            &[&ENV_KEY, &run_id],
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
                     WHERE q.env_key=r.env_key AND q.run_id=r.run_id) \
                   FROM {SCHEMA}.runs r WHERE env_key=$1 AND run_id=$2"
            ),
            &[&ENV_KEY, &run_id],
        )
        .await?;
    assert_eq!(row.get::<_, String>(0), status);
    assert!(row.get::<_, bool>(1));
    Ok(())
}

pub fn ready_run(result: ProductionClaimResult) -> String {
    match result {
        ProductionClaimResult::Ready { run_id, .. } => run_id,
        ProductionClaimResult::Empty => panic!("expected ready claim, got Empty"),
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
    let executor = wamn_project_state::PlatformComponent::Executor;
    for component in [COMPONENT, ROLLED_COMPONENT] {
        plugin.set_user_id(component, &executor.principal_id().to_string())?;
        plugin.set_operation(component, executor.principal_name())?;
    }
    plugin.set_env_key(COMPONENT, ENV_KEY)?;
    plugin.set_schema(COMPONENT, SCHEMA)?;
    plugin.set_runner(COMPONENT, COMPONENT)?;
    plugin.set_release_identity(
        COMPONENT,
        ReleaseIdentity::for_test(wamn_catalog::ManifestDigest::parse(POD_MANIFEST_DIGEST)?),
    )?;
    plugin.set_env_key(ROLLED_COMPONENT, ENV_KEY)?;
    plugin.set_schema(ROLLED_COMPONENT, SCHEMA)?;
    plugin.set_runner(ROLLED_COMPONENT, ROLLED_COMPONENT)?;
    plugin.set_release_identity(
        ROLLED_COMPONENT,
        ReleaseIdentity::for_test(wamn_catalog::ManifestDigest::parse(ROLLED_MANIFEST_DIGEST)?),
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
