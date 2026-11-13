//! The fixture of the control host route tests: the control store, one org with
//! two projects and one environment each, and the org's `control` login
//! (docs/architecture/execution.md#host-run-routes).
//!
//! It also builds the project databases of the two environments, with the
//! administration logins that the control host reads from its mounted Secret.
//!
//! `control_route_live` and `tests/integration`'s `control_client_live` include it.

use sqlx::PgConnection as Client;
use sqlx::Row as _;

use wamn_control_provision::{
    PlatformComponent, WorkloadRoleFamily, WorkloadRoleIdentity, workload_login,
};

pub(super) const ORG: &str = "org-a";
pub(super) const PASSWORD: &str = "control-route-test-only";

pub(super) async fn connect(url: &str) -> anyhow::Result<Client> {
    let client = wamn_postgres::connect(url).await?;

    Ok(client)
}

/// The control store that `provision-system` installs, one org with two
/// projects and one environment each, and the org's `control` login.
pub(super) async fn install(admin: &mut Client, admin_url: &str) -> anyhow::Result<String> {
    let database: String = sqlx::query("SELECT current_database()::text")
        .fetch_one(&mut *admin)
        .await?
        .get(0);
    sqlx::raw_sql("CREATE ROLE wamn_system NOLOGIN NOINHERIT")
        .execute(&mut *admin)
        .await
        .map(drop)?;
    wamn_environment_control::provision_system::install_control_store(admin).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "SET ROLE wamn_system; SET app.user_id = '{}'",
            wamn_control_provision::PlatformComponent::Provisioning.principal_id()
        ))
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)?;
    sqlx::raw_sql("INSERT INTO registry.orgs (id, placement_type, pool_cluster) \
               VALUES ('org-a', 'pooled', 'wamn-pg'); \
             INSERT INTO registry.env_policies \
               (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image) \
               VALUES ('org-a', 'dev', '\"own\"', 0, 1, '1Gi', '1', '1Gi', 'postgres:18'); \
             INSERT INTO registry.projects (org, id) VALUES ('org-a', 'billing'), ('org-a', 'shop'); \
             WITH parent_environment AS (INSERT INTO registry.environments (org,env,policy_name) VALUES ('org-a','dev','dev'),('org-a','dev','dev') ON CONFLICT (org,env) DO NOTHING) INSERT INTO registry.environment_projects (org,project,env,secret_name,instance_suffix) VALUES ('org-a','billing','dev','s1','aaaaaaa1'),('org-a','shop','dev','s2','aaaaaaa2'); \
             RESET ROLE").execute(&mut *admin).await.map(drop)?;
    let role = workload_login(
        WorkloadRoleFamily::Control,
        WorkloadRoleIdentity::Org {
            org: ORG,
            database: &database,
        },
    )?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::Control,
            &database,
            &role,
            PASSWORD,
        ))
            .to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)?;
    let mut url = url::Url::parse(admin_url)?;
    url.set_username(&role)
        .map_err(|()| anyhow::anyhow!("set fixture login"))?;
    url.set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("set fixture password"))?;
    Ok(url.into())
}

/// One environment's project database on the test server, with the catalog,
/// the application schema and one platform row, and its administration
/// login minted by the production prepare. The login goes
/// into `logins` under the key its control patch names, as the mounted
/// Secret shows it.
pub(super) async fn project_database(
    admin: &mut Client,
    admin_url: &str,
    system_url: &str,
    project: &str,
    instance: &str,
    logins: &std::path::Path,
) -> anyhow::Result<Client> {
    use wamn_environment_control::provision_project_env;
    let mut database =
        wamn_control_provision::project_env_database_name(ORG, project, "dev", instance);
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&provision_project_env::role_posture_sql()).to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE \"{database}\" OWNER wamn_system"
    )))
    .execute(&mut *admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&provision_project_env::privilege_sql(&mut database)).to_string(),
    ))
    .execute(&mut *admin)
    .await
    .map(drop)?;
    let mut target_url = url::Url::parse(admin_url)?;
    target_url.set_path(&format!("/{database}"));
    let mut target = connect(target_url.as_str()).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (" \
             CREATE SCHEMA wamn_run;")
            .to_string(),
    ))
    .execute(&mut target)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "{} {} {}",
            wamn_control_provision::sql::ensure_platform_group_role_sql(),
            wamn_control_provision::sql::ensure_db_owner_role_sql(),
            wamn_control_provision::sql::ensure_workload_acl_role_sql(
                WorkloadRoleFamily::AuditRetention
            ),
        ))
            .to_string(),
    ))
    .execute(&mut target)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "SET ROLE wamn_system; {}",
        wamn_catalog::CATALOG_SCHEMA_SQL
    )))
    .execute(&mut target)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "{} RESET ROLE;",
            include_str!("../../../../../deploy/sql/app-schema.sql")
        ))
            .to_string(),
    ))
    .execute(&mut target)
    .await
    .map(drop)?;
    sqlx::query(
        "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:control-route-fixture', false)",
    )
    .bind(&PlatformComponent::Provisioning.principal_id().to_string())
    .execute(&mut target)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::query(
        "INSERT INTO platform.\"user\" (id, type, email, display_name) \
             VALUES ($1::text::uuid, 'platform', 'provisioning@example.test', $2)",
    )
    .bind(&PlatformComponent::Provisioning.principal_id().to_string())
    .bind(&PlatformComponent::Provisioning.principal_name())
    .execute(&mut target)
    .await
    .map(|result| result.rows_affected())?;

    let login = wamn_test_infrastructure::credentials::database_login(
        system_url,
        Some(target_url.as_str()),
        ORG,
        project,
        "dev",
        WorkloadRoleFamily::Administration,
    )
    .await?;
    std::fs::write(
        logins.join(wamn_registry::project_env_key(ORG, project, "dev")),
        login,
    )?;
    Ok(target)
}

/// The project databases of `billing/dev` and `shop/dev`, with their logins
/// in a fresh directory named for `test`.
pub(super) async fn environments(
    admin: &mut Client,
    admin_url: &str,
    test: &str,
) -> anyhow::Result<(std::path::PathBuf, Client, Client)> {
    let logins = std::env::temp_dir().join(format!(
        "wamn-control-administration-{test}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&logins);
    std::fs::create_dir_all(&logins)?;
    let billing =
        project_database(admin, admin_url, admin_url, "billing", "aaaaaaa1", &logins).await?;
    let shop = project_database(admin, admin_url, admin_url, "shop", "aaaaaaa2", &logins).await?;
    Ok((logins, billing, shop))
}
