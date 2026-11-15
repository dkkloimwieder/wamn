//! Live executor access and lease authority checks.

use anyhow::Context as _;
use sqlx::PgConnection as Client;
use sqlx::Row as _;

use url::Url;
use wamn_capability_authority::identity::ReleaseIdentity;
use wamn_connections::production_lease::{
    ProductionCompletion, ProductionCompletionResult, ProductionLeaseRenewal,
    ProductionLeaseResult, ProductionReapResult,
};
use wamn_control_provision::{WorkloadRoleFamily, sql};
use wamn_postgres_capability::{ClassCredentials, WamnPostgres, WamnPostgresConfig};

/// The generation login every leg runs as.
///
/// Named with `ExecutorPlatform`'s frozen SHORT generation prefix: the family's
/// stable role name does not fit the 63-byte identifier cap once an identity digest
/// and an A/B suffix are appended, so a login that reads like a real generation
/// has to start here.
const GENERATION: &str = "wamn_exec_platform_surface_live";
const GENERATION_PASSWORD: &str = "executor-surface-test-password";

const PROJECT_ENV_KEY: &str = "executor-surface--prod--cat";
const PACKAGE_ID: &str = "cat";
const PACKAGE_VERSION: &str = "1.0.0";
const ENVIRONMENT: &str = "prod";
const WIRING_ID: &str = "wiring-a";
const WIRING_VERSION: i32 = 1;

fn digest(fill: &str) -> wamn_digest::Digest {
    format!("sha256:{}", fill.repeat(64)).parse().unwrap()
}

async fn connect(url: &str) -> anyhow::Result<Client> {
    wamn_postgres::connect(url).await
}

/// Rewrite an admin URL onto the generation's credential.
fn generation_url(admin: &str) -> anyhow::Result<String> {
    let mut url = Url::parse(admin)?;
    url.set_username(GENERATION)
        .map_err(|()| anyhow::anyhow!("set the generation username"))?;
    url.set_password(Some(GENERATION_PASSWORD))
        .map_err(|()| anyhow::anyhow!("set the generation password"))?;
    Ok(url.into())
}

/// Open one transaction shaped like `begin_with_identity` does for this class.
///
/// The three plan-supply statements take the project as `$1` and read no GUC, so
/// the identity is not what fences them; it is set because it IS what the host
/// injects, and because the `run_queue` leg below depends on it.
async fn begin_bound(client: &mut Client) -> anyhow::Result<()> {
    sqlx::raw_sql("BEGIN")
        .execute(&mut *client)
        .await
        .map(drop)?;
    sqlx::query("INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1::text::uuid,'','fixture',$2)").bind(&wamn_control_provision::PlatformComponent::Executor.principal_id().to_string()).bind(&wamn_control_provision::PlatformComponent::Executor.principal_name()).execute(&mut *client).await.map(|result| result.rows_affected())?;
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn executor_platform_surface_live(pool: sqlx::PgPool) -> anyhow::Result<()> {
    const COMPONENT: &str = "executor-authority-test";

    let _lock = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let admin_url = test_database.as_str();
    let mut admin = connect(admin_url).await?;
    let database: String = sqlx::query("SELECT current_database()::text")
        .fetch_one(&mut admin)
        .await?
        .get(0);

    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    let catalog_ddl = wamn_catalog::CATALOG_SCHEMA_SQL;
    let app_schema = wamn_control_provision::APP_SCHEMA_SQL;
    let run_state_ddl = std::fs::read_to_string(format!("{root}/deploy/sql/run-state.sql"))
        .context("read run-state DDL")?;
    let run_queue_ddl = std::fs::read_to_string(format!("{root}/deploy/sql/run-queue.sql"))
        .context("read run-queue DDL")?;

    // THE GENERATION IS DROPPED BEFORE IT IS MINTED, INSIDE AN EXISTENCE CHECK.
    // Roles are CLUSTER-wide, so a login left behind by an earlier run of this
    // suite satisfies every `IF NOT EXISTS` in the builder and would let a
    // MUTATED builder pass on the previous run's role.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $$ DECLARE role_name text; BEGIN \
               FOREACH role_name IN ARRAY ARRAY[ \
                 'wamn_app', \
                 'wamn_executor_platform','wamn_http_admitter','{GENERATION}' \
               ] LOOP \
                 IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname=role_name) THEN \
                   EXECUTE format('DROP OWNED BY %I', role_name); \
                   EXECUTE format('DROP ROLE %I', role_name); \
                 END IF; \
               END LOOP; \
             END $$; \
             CREATE ROLE wamn_query NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOBYPASSRLS; CREATE ROLE wamn_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
              \
              \
             BEGIN; {catalog_ddl} {run_state_ddl} {run_queue_ddl} {app_schema} COMMIT;"
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .context("install the run and catalog planes")?;
    // ONE BUILDER CALL AND NOTHING ELSE. `test_database::login_sql`
    // carries `grant_executor_platform_surface_sql` as its stable surface, the
    // `wamn_platform` group edge, the generation role, its password and its
    // `CONNECT`. Every plan-supply leg below therefore stands on exactly what
    // provisioning emits; no grant is manufactured here.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::ExecutorPlatform,
            &database,
            GENERATION,
            GENERATION_PASSWORD,
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .context("provision the executor-platform generation")?;
    // The wrong-membership arm names a role that must EXIST: `pg_has_role`
    // raises on an unknown role, which the probe reports as an UNAVAILABLE
    // probe rather than a mismatch, and the arm would then pass for the wrong
    // reason. This is the family's own builder, not a hand-rolled role.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&sql::ensure_workload_acl_role_sql(WorkloadRoleFamily::HttpAdmitter)).to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .context("ensure the http-admitter acl role")?;

    // THE PRINCIPAL, ASSERTED BEFORE ANY LEG RUNS. A superuser or a BYPASSRLS
    // login satisfies every read below and every `pg_has_role` above while
    // showing nothing about either, and it would do it SILENTLY.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "DO $$ BEGIN \
               ASSERT EXISTS ( \
                 SELECT FROM pg_catalog.pg_authid \
                  WHERE rolname = '{GENERATION}' \
                    AND rolcanlogin AND NOT rolsuper AND NOT rolcreatedb \
                    AND NOT rolcreaterole AND rolinherit AND NOT rolreplication \
                    AND NOT rolbypassrls AND rolpassword IS NOT NULL), \
                 'the surface principal must be a minted, non-bypassing generation'; \
               ASSERT EXISTS ( \
                 SELECT FROM pg_catalog.pg_auth_members AS membership \
                 JOIN pg_catalog.pg_roles AS parent ON parent.oid = membership.roleid \
                 JOIN pg_catalog.pg_roles AS child ON child.oid = membership.member \
                  WHERE parent.rolname = 'wamn_executor_platform' \
                    AND child.rolname = '{GENERATION}' \
                    AND NOT membership.admin_option AND membership.inherit_option \
                    AND NOT membership.set_option), \
                 'the generation must INHERIT the stable role, not hold its grants'; \
               ASSERT pg_catalog.pg_has_role('{GENERATION}', 'wamn_platform', 'USAGE'), \
                 'the generation must match the permissive platform base grants arm'; \
               ASSERT NOT pg_catalog.pg_has_role('{GENERATION}', 'wamn_app', 'USAGE'), \
                 'the generation must not reach the guest role'; \
               ASSERT NOT pg_catalog.pg_has_role('{GENERATION}', 'wamn_http_admitter', 'USAGE'), \
                 'the generation must not reach the callable-http role'; \
             END $$;"
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .context("assert the generation identity")?;

    let component_digest = digest("a");
    let imports_fingerprint = digest("b");
    let artifact_digest = digest("f");
    // The release facts the runtime reads from the bytes: package membership,
    // the wiring membership and the component closure, each component with
    // its descriptor.
    let manifest_body = format!(
        r#"{{"components":[{{"descriptor":{{"component":"entity","component-digest":"{component_digest}","connections":[{{"requirement-type":"http","store-alias":"a-store"}}],"effects":[],"imports":[],"imports-fingerprint":"{imports_fingerprint}","interface-version":"0.1","operations":{{"create":{{"input-ports":[],"output-ports":[],"parameters":[]}}}}}},"package-id":"{PACKAGE_ID}"}}],"release":{{"packages":[{{"package-id":"{PACKAGE_ID}","package-version":"{PACKAGE_VERSION}"}}]}}}}"#,
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "BEGIN; INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES('770df186-ac15-579e-b46b-c297cae2011b','','fixture','admin:plan-supply-fixture'); \
             INSERT INTO catalog.packages ( project_env_key,package_id,package_version,artifact_digest) VALUES ( '{PROJECT_ENV_KEY}','{PACKAGE_ID}','{PACKAGE_VERSION}','{artifact_digest}'); \
             INSERT INTO catalog.releases \
               (project_env_key,manifest_digest,canonical_bytes) \
             SELECT '{PROJECT_ENV_KEY}', 'sha256:' || encode(sha256(bytes), 'hex'), bytes \
               FROM (SELECT convert_to('{manifest_body}', 'UTF8') AS bytes) AS frozen; \
             INSERT INTO platform.integration (id,integration_type,name,status) \
             VALUES ('00000000-0000-4000-8000-000000000011','http','a-store','active'); \
             INSERT INTO platform.connection (id,integration_id,integration_type,status) \
             VALUES ('00000000-0000-4000-8000-000000000012','00000000-0000-4000-8000-000000000011','http','active'); \
             INSERT INTO catalog.connection_requirement (project_env_key,component_digest,package_id,name,interface,mode) \
             VALUES ('{PROJECT_ENV_KEY}','{component_digest}','{PACKAGE_ID}','a-store','wamn:connection/http@0.1.0','write'); \
             INSERT INTO catalog.release_connection (project_env_key,manifest_digest,connection_id,version) \
             SELECT '{PROJECT_ENV_KEY}',manifest_digest,'00000000-0000-4000-8000-000000000012',1 FROM catalog.releases; \
             INSERT INTO catalog.connection_binding (project_env_key,manifest_digest,component_digest,package_id,name,connection_id) \
             SELECT '{PROJECT_ENV_KEY}',manifest_digest,'{component_digest}','{PACKAGE_ID}','a-store','00000000-0000-4000-8000-000000000012' FROM catalog.releases; \
             INSERT INTO wamn_run.runs (project_env_key, run_id, wiring_hash, package_id, manifest_digest, environment, wiring_id, wiring_version, status, input_json, trigger_source, execution_principal_id, execution_role, invoked_by, invoked_by_user_id, node_digests_json) SELECT '{PROJECT_ENV_KEY}', 'run-1', 'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd', '{PACKAGE_ID}', manifest_digest, '{ENVIRONMENT}', '{WIRING_ID}', {WIRING_VERSION}, 'dispatched', '{{}}', 'manual', '00000000-0000-0000-0000-000000000001', 'admin', 'manual', '00000000-0000-0000-0000-000000000001', '{{}}' FROM catalog.releases; \
             INSERT INTO wamn_run.run_queue (project_env_key,run_id) VALUES ('{PROJECT_ENV_KEY}','run-1'); COMMIT;",
        ))).execute(&mut admin).await.map(drop)
        .context("seed the plan-supply fixture")?;

    let manifest_digest = wamn_digest::bytes_sha256(manifest_body.as_bytes());

    // ---- PLAN SUPPLY, LIVE UNDER THE CREDENTIAL ---------------------------
    let platform_url = generation_url(admin_url)?;
    let mut generation = connect(&platform_url).await?;
    begin_bound(&mut generation).await?;

    // *** `has_table_privilege` IS BLIND TO RLS. *** The ACL says this
    // principal may SELECT `wamn_run.run_queue`; `run_queue_project_env_key` carries no
    // `TO` clause and keys on `app.project_env_key`, so the SAME connection reads one
    // row with the identity injected and ZERO without it. This is why every arm in
    // this file asserts rows rather than privileges.
    let bound_rows: i64 = sqlx::query("SELECT count(*) FROM wamn_run.run_queue")
        .fetch_one(&mut generation)
        .await?
        .get(0);
    assert_eq!(bound_rows, 1, "the bound session read no queue row");
    sqlx::raw_sql("COMMIT")
        .execute(&mut generation)
        .await
        .map(drop)?;
    let unbound_rows: i64 = sqlx::query("SELECT count(*) FROM wamn_run.run_queue")
        .fetch_one(&mut generation)
        .await?
        .get(0);
    assert_eq!(
        unbound_rows, 0,
        "an unbound session read the queue that the project identity is supposed to fence"
    );
    let acl_says_yes: bool = sqlx::query(
        "SELECT pg_catalog.has_table_privilege( \
               current_user, 'wamn_run.run_queue', 'SELECT')",
    )
    .fetch_one(&mut generation)
    .await?
    .get(0);
    assert!(
        acl_says_yes,
        "the ACL blindness leg needs the privilege it is blind about"
    );

    // Warm the real executor pool before revoking membership on its existing connection.
    let plugin = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(platform_url)),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 5_000,
        statement_timeout_ms: 10_000,
    })?;
    plugin.set_user_id(
        COMPONENT,
        &wamn_control_provision::PlatformComponent::Executor
            .principal_id()
            .to_string(),
    )?;
    plugin.set_operation(
        COMPONENT,
        &wamn_control_provision::PlatformComponent::Executor.principal_name(),
    )?;
    plugin.set_project_env_key(COMPONENT, PROJECT_ENV_KEY)?;
    plugin.set_schema(COMPONENT, "wamn_run")?;
    plugin.set_runner(COMPONENT, COMPONENT)?;
    // A production host carries its release digest and leases only the runs
    // pinned to it (docs/architecture/execution.md#environment-workflows).
    plugin.set_release_identity(COMPONENT, ReleaseIdentity::for_test(manifest_digest))?;
    assert_eq!(
        wamn_connections::production_lease::ProductionLeases::new(&plugin)
            .renew(COMPONENT, "absent", 1, 30_000)
            .await?,
        ProductionLeaseRenewal::FenceLost
    );
    let packages = [PACKAGE_ID.parse().unwrap()];
    let completion = ProductionCompletion::completed(serde_json::json!({"done": true}));
    assert_eq!(
        wamn_connections::production_lease::ProductionLeases::new(&plugin)
            .reap_exhausted(COMPONENT, &packages, ENVIRONMENT, 0)
            .await?,
        ProductionReapResult::Empty
    );
    let ProductionLeaseResult::Ready {
        run_id,
        lease_generation,
        ..
    } = wamn_connections::production_lease::ProductionLeases::new(&plugin)
        .lease_next(COMPONENT, &packages, ENVIRONMENT, 30_000)
        .await?
    else {
        anyhow::bail!("the restored executor must lease its unchanged run");
    };
    assert_eq!(run_id, "run-1");
    assert_eq!(lease_generation, 1);
    assert_eq!(
        wamn_connections::production_lease::ProductionLeases::new(&plugin)
            .renew(COMPONENT, &run_id, lease_generation, 30_000)
            .await?,
        ProductionLeaseRenewal::Renewed
    );
    assert_eq!(
        wamn_connections::production_lease::ProductionLeases::new(&plugin)
            .complete(COMPONENT, &run_id, lease_generation, &completion)
            .await?,
        ProductionCompletionResult::Terminalized
    );

    Ok(())
}
