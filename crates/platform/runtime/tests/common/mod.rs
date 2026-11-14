//! Shared fixtures for production queue tests.

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _, Statement as _};
use std::sync::Arc;

use url::Url;
use wamn_identity_protocol::AuthorityClass;
use wamn_runtime::plugins::wamn_postgres::{
    ClassCredentials, ProductionLeaseResult, ReleaseIdentity, WamnPostgres, WamnPostgresConfig,
};

pub const PROJECT_ENV_KEY: &str = "lease-live";
pub const COMPONENT: &str = "lease-live-runner";
pub const PACKAGE_ID: &str = "cat_main";
pub const ENVIRONMENT: &str = "test";
/// A second pod carrying a different release — the mismatch case.
pub const ROLLED_COMPONENT: &str = "lease-live-runner-next";
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
pub const WIRING_ID: &str = "lease-live-wiring";
pub const WIRING_VERSION: i32 = 1;
pub const EMPTY_HASH: &str =
    "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";
const EXECUTOR_PASSWORD: &str = "lease-live-executor-password";

pub async fn connect(url: &str) -> anyhow::Result<Client> {
    let mut client = wamn_postgres::connect(url).await?;

    let actor = wamn_identity_protocol::PlatformComponent::Provisioning;
    sqlx::query("SELECT set_config('app.user_id', $1, false), set_config('app.operation', 'admin:lease-fixture', false)").bind(&actor.principal_id().to_string()).execute(&mut client).await.map(|result| result.rows_affected())?;
    Ok(client)
}

pub fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

const RUN_STATE_SQL: &str = include_str!("../../../../../deploy/sql/run-state.sql");
const RUN_QUEUE_SQL: &str = include_str!("../../../../../deploy/sql/run-queue.sql");

/// Apply the run plane of record on the project base grants, with the two releases
/// the pods mount.
pub async fn install_schema(client: &mut Client) -> anyhow::Result<()> {
    sqlx::raw_sql(RUN_STATE_SQL)
        .execute(&mut *client)
        .await
        .map(drop)?;
    sqlx::raw_sql(RUN_QUEUE_SQL)
        .execute(&mut *client)
        .await
        .map(drop)?;
    sqlx::raw_sql(wamn_control_provision::APP_SCHEMA_SQL)
        .execute(&mut *client)
        .await
        .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "INSERT INTO catalog.releases (project_env_key, manifest_digest, canonical_bytes) \
             VALUES ('{PROJECT_ENV_KEY}', '{POD_MANIFEST_DIGEST}', '{POD_MANIFEST_BYTES}'), \
                    ('{PROJECT_ENV_KEY}', '{ROLLED_MANIFEST_DIGEST}', '{ROLLED_MANIFEST_BYTES}');"
        ))
            .to_string(),
    ))
    .execute(&mut *client)
    .await
    .map(drop)?;
    Ok(())
}

/// Mint the executor generation separately from the fixture's administrator.
async fn install_executor(
    client: &mut Client,
    admin_url: &str,
) -> anyhow::Result<(String, String)> {
    use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, workload_login};

    let database: String = sqlx::query("SELECT current_database()::text")
        .fetch_one(&mut *client)
        .await?
        .get(0);
    let family = WorkloadRoleFamily::ExecutorPlatform;
    let role = workload_login(
        family,
        WorkloadRoleIdentity::ProjectEnvironment {
            org: "lease-live-org",
            project: "lease-live-project",
            environment: ENVIRONMENT,
            database: &database,
        },
    )?;
    // The production builder: the stable executor surface, the `wamn_platform`
    // group edge, the generation login and its `CONNECT`.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::login_sql(
            family,
            &database,
            &role,
            EXECUTOR_PASSWORD,
        ))
            .to_string(),
    ))
    .execute(&mut *client)
    .await
    .map(drop)?;
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

/// Seed an admitted, queued run.
pub async fn seed_run(client: &mut Client, run_id: &str, package_id: &str) -> anyhow::Result<()> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO {SCHEMA}.runs \
                   (project_env_key,run_id,wiring_hash,status,package_id,manifest_digest, \
                    environment,wiring_id,wiring_version,input_json,trigger_source, execution_principal_id, execution_role, invoked_by, invoked_by_user_id, node_digests_json) \
                 VALUES ($1,$2,'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','dispatched',$3,'{POD_MANIFEST_DIGEST}','test',$4,$5, \
                         '{{\"input\":true}}','http')"
            ))).bind(&PROJECT_ENV_KEY).bind(&run_id).bind(&package_id).bind(&WIRING_ID).bind(&WIRING_VERSION).execute(&mut *client).await.map(|result| result.rows_affected())?;
    sqlx::query(sqlx::AssertSqlSafe(
        (&format!(
            "INSERT INTO {SCHEMA}.run_queue \
                   (project_env_key,run_id,available_at) \
                 VALUES ($1,$2,'2000-01-01 00:00:00+00')"
        ))
            .to_string(),
    ))
    .bind(&PROJECT_ENV_KEY)
    .bind(&run_id)
    .execute(&mut *client)
    .await
    .map(|result| result.rows_affected())?;
    Ok(())
}

pub async fn expire_effect_run(client: &mut Client, run_id: &str) -> anyhow::Result<()> {
    sqlx::query(sqlx::AssertSqlSafe(
        (&format!(
            "UPDATE {SCHEMA}.run_queue SET lease_expires_at='2000-01-01' \
                  WHERE project_env_key=$1 AND run_id=$2"
        ))
            .to_string(),
    ))
    .bind(&PROJECT_ENV_KEY)
    .bind(&run_id)
    .execute(&mut *client)
    .await
    .map(|result| result.rows_affected())?;
    Ok(())
}

pub async fn seed_exhausted_run(client: &mut Client, run_id: &str) -> anyhow::Result<()> {
    seed_run(client, run_id, PACKAGE_ID).await?;
    sqlx::query(sqlx::AssertSqlSafe(
        (&format!(
            "UPDATE {SCHEMA}.run_queue \
                    SET lease_owner='dead', lease_expires_at='2000-01-01', \
                        attempts=max_attempts \
                  WHERE project_env_key=$1 AND run_id=$2"
        ))
            .to_string(),
    ))
    .bind(&PROJECT_ENV_KEY)
    .bind(&run_id)
    .execute(&mut *client)
    .await
    .map(|result| result.rows_affected())?;
    Ok(())
}

/// The crash-evidence attempt count a run's queue row carries.
pub async fn queue_attempts(client: &mut Client, run_id: &str) -> anyhow::Result<i32> {
    Ok(sqlx::query(sqlx::AssertSqlSafe(
        (&format!(
            "SELECT attempts FROM {SCHEMA}.run_queue WHERE project_env_key=$1 AND run_id=$2"
        ))
            .to_string(),
    ))
    .bind(&PROJECT_ENV_KEY)
    .bind(&run_id)
    .fetch_one(&mut *client)
    .await?
    .get(0))
}

/// The admission-pinned release digest a run carries.
pub async fn release_record(client: &mut Client, run_id: &str) -> anyhow::Result<Option<String>> {
    let row = sqlx::query(sqlx::AssertSqlSafe(
        (&format!(
            "SELECT manifest_digest \
                   FROM {SCHEMA}.runs WHERE project_env_key=$1 AND run_id=$2"
        ))
            .to_string(),
    ))
    .bind(&PROJECT_ENV_KEY)
    .bind(&run_id)
    .fetch_one(&mut *client)
    .await?;
    Ok(row.get(0))
}

pub async fn assert_terminal_status_dequeued(
    client: &mut Client,
    run_id: &str,
    status: &str,
) -> anyhow::Result<()> {
    let row = sqlx::query(sqlx::AssertSqlSafe(
        (&format!(
            "SELECT status, NOT EXISTS ( \
                    SELECT 1 FROM {SCHEMA}.run_queue q \
                     WHERE q.project_env_key=r.project_env_key AND q.run_id=r.run_id) \
                   FROM {SCHEMA}.runs r WHERE project_env_key=$1 AND run_id=$2"
        ))
            .to_string(),
    ))
    .bind(&PROJECT_ENV_KEY)
    .bind(&run_id)
    .fetch_one(&mut *client)
    .await?;
    assert_eq!(row.get::<String, _>(0), status);
    assert!(row.get::<bool, _>(1));
    Ok(())
}

pub fn ready_run(result: ProductionLeaseResult) -> String {
    match result {
        ProductionLeaseResult::Ready { run_id, .. } => run_id,
        ProductionLeaseResult::Empty => panic!("expected ready lease, got Empty"),
    }
}

/// The `application_name` the runtime pool carries.
///
/// `wait_for_advisory_wait` selects the blocked reaper backend by this exact
/// string, so it is fixture-wide rather than per-suite.
pub const RUNTIME_APPLICATION_NAME: &str = "production-lease-live-runtime";

/// Everything a live lease suite needs, built once per suite.
pub struct LiveFixture {
    pub admin: Client,
    pub plugin: Arc<WamnPostgres>,
    executor_role: String,
}

/// Install the schema, the executor credential, and pod identities.
pub async fn install_fixture(url: &str) -> anyhow::Result<LiveFixture> {
    let mut admin = connect(url).await?;
    install_schema(&mut admin).await?;
    let (runtime_url, executor_role) = install_executor(&mut admin, url).await?;

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
    let executor = wamn_identity_protocol::PlatformComponent::Executor;
    for component in [COMPONENT, ROLLED_COMPONENT] {
        plugin.set_user_id(component, &executor.principal_id().to_string())?;
        plugin.set_operation(component, &executor.principal_name())?;
    }
    plugin.set_project_env_key(COMPONENT, PROJECT_ENV_KEY)?;
    plugin.set_schema(COMPONENT, SCHEMA)?;
    plugin.set_runner(COMPONENT, COMPONENT)?;
    plugin.set_release_identity(
        COMPONENT,
        ReleaseIdentity::for_test(wamn_catalog::ManifestDigest::parse(POD_MANIFEST_DIGEST)?),
    )?;
    plugin.set_project_env_key(ROLLED_COMPONENT, PROJECT_ENV_KEY)?;
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
        mut admin,
        plugin,
        executor_role,
    } = fixture;
    drop(plugin);
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP OWNED BY {executor_role}; DROP ROLE {executor_role};"
    )))
    .execute(&mut admin)
    .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP SCHEMA {SCHEMA} CASCADE; DROP SCHEMA catalog CASCADE;"
    )))
    .execute(&mut admin)
    .await
    .map(drop)?;
    Ok(())
}
