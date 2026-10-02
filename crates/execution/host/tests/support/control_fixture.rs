//! The fixture of the control host route tests: the system schema, one org
//! with two projects and one environment each, and the org's `control`
//! login (docs/plan/platform-ui.md §4.4 and §4.5).
//!
//! It also builds the project databases of the two environments, with the
//! administration logins that the control host reads from its mounted Secret.
//!
//! `control_route_live` and `tests/integration`'s `control_client_live` and
//! `shell_browser_live` include it.

use tokio_postgres::{Client, NoTls};
use wamn_control_provision::{
    CredentialGeneration, PlatformComponent, WorkloadRoleFamily, WorkloadRoleScope, sql,
    workload_generation_role,
};

pub(super) const ORG: &str = "org-a";
pub(super) const PASSWORD: &str = "control-route-test-only";

pub(super) async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move { connection.await.expect("fixture database connection") });
    Ok(client)
}

/// The system schema as `wamn_system`, one org with two projects and one
/// environment each, and the org's `control` login.
pub(super) async fn install(admin: &Client, admin_url: &str) -> anyhow::Result<String> {
    admin
        .batch_execute(wamn_control_provision::sql::ensure_db_owner_role_sql())
        .await?;
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    admin
        .batch_execute(&format!(
            "CREATE ROLE wamn_system NOLOGIN; GRANT CREATE ON DATABASE \"{database}\" TO wamn_system; \
             SET ROLE wamn_system"
        ))
        .await?;
    admin
        .batch_execute(wamn_control_provision::SYSTEM_SCHEMA_SQL)
        .await?;
    admin
        .batch_execute(
            "INSERT INTO registry.orgs (id, placement_type, pool_cluster) \
               VALUES ('org-a', 'pooled', 'wamn-pg'); \
             INSERT INTO registry.env_policies \
               (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image) \
               VALUES ('org-a', 'dev', '\"own\"', 0, 1, '1Gi', '1', '1Gi', 'postgres:18'); \
             INSERT INTO registry.projects (org, id) VALUES ('org-a', 'billing'), ('org-a', 'shop'); \
             INSERT INTO registry.project_envs (org, project, env, secret_name, instance_suffix) \
               VALUES ('org-a', 'billing', 'dev', 's1', 'aaaaaaa1'), \
                      ('org-a', 'shop', 'dev', 's2', 'aaaaaaa2'); \
             RESET ROLE",
        )
        .await?;
    let role = workload_generation_role(
        WorkloadRoleFamily::Control,
        WorkloadRoleScope::Org {
            org: ORG,
            database: &database,
        },
        CredentialGeneration::A,
    )?;
    admin
        .batch_execute(&sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::Control,
            &database,
            &role,
            PASSWORD,
            "2099-01-01T00:00:00Z",
        ))
        .await?;
    let mut url = url::Url::parse(admin_url)?;
    url.set_username(&role)
        .map_err(|()| anyhow::anyhow!("set fixture login"))?;
    url.set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("set fixture password"))?;
    Ok(url.into())
}

/// One environment's project database on the test server, with the catalog,
/// the application schema and one platform row of `tenant`, and its
/// administration login minted by the production prepare. The login goes
/// into `logins` under the key its control patch names, as the mounted
/// Secret shows it.
pub(super) async fn project_database(
    admin: &Client,
    admin_url: &str,
    system_url: &str,
    project: &str,
    instance: &str,
    tenant: &str,
    logins: &std::path::Path,
) -> anyhow::Result<Client> {
    use wamn_control::provision_project_env::{
        self, WorkloadActionRequest, WorkloadActionVerb, WorkloadGenerationAction,
    };
    let database = wamn_control_provision::project_env_database_name(ORG, project, "dev", instance);
    admin
        .batch_execute(&provision_project_env::role_posture_sql())
        .await?;
    admin
        .batch_execute(&format!("CREATE DATABASE \"{database}\""))
        .await?;
    admin
        .batch_execute(&provision_project_env::privilege_sql(&database))
        .await?;
    let mut target_url = url::Url::parse(admin_url)?;
    target_url.set_path(&format!("/{database}"));
    let target = connect(target_url.as_str()).await?;
    target
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles \
                                        WHERE rolname = 'wamn_scenario_author') THEN \
               CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOBYPASSRLS; \
             END IF; END $$; \
             CREATE SCHEMA wamn_run;",
        )
        .await?;
    target
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await?;
    target
        .batch_execute(include_str!("../../../../../deploy/sql/app-schema.sql"))
        .await?;
    target
        .execute(
            "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:control-route-fixture', false)",
            &[&PlatformComponent::Provisioning.principal_id().to_string()],
        )
        .await?;
    target
        .execute(
            "INSERT INTO app_system.users (tenant_id, id, type, email, display_name) \
             VALUES ($1, $2::text::uuid, 'platform', 'provisioning@example.test', $3)",
            &[
                &tenant,
                &PlatformComponent::Provisioning.principal_id().to_string(),
                &PlatformComponent::Provisioning.principal_name(),
            ],
        )
        .await?;

    let secret = logins.join(format!("{project}.secret.json"));
    let patch = logins.join(format!("{project}.patch.json"));
    provision_project_env::run_workload_action(&WorkloadActionRequest {
        org: ORG.to_owned(),
        project: project.to_owned(),
        env: "dev".to_owned(),
        tenant: Some(tenant.to_owned()),
        system_database_url: Some(system_url.to_owned()),
        target_admin_database_url: Some(target_url.to_string()),
        cluster: None,
        db_host: target_url.host_str().map(str::to_owned),
        db_port: target_url.port_or_known_default().unwrap_or(5432),
        namespace: "hosts".to_owned(),
        action: WorkloadGenerationAction {
            family: WorkloadRoleFamily::Administration,
            verb: WorkloadActionVerb::Prepare,
            generation: CredentialGeneration::A,
        },
        secret: Some(secret.clone()),
        emit_role_sql: None,
        control_administration_patch: Some(patch.clone()),
    })
    .await?;
    let patch: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&patch)?)?;
    for (key, url) in patch["stringData"]
        .as_object()
        .expect("the patch sets one key")
    {
        std::fs::write(logins.join(key), url.as_str().expect("the key holds a URL"))?;
    }
    std::fs::remove_file(secret)?;
    Ok(target)
}

/// The project databases of `billing/dev` and `shop/dev`, with their logins
/// in a fresh directory named for `test`.
pub(super) async fn environments(
    admin: &Client,
    admin_url: &str,
    test: &str,
) -> anyhow::Result<(std::path::PathBuf, Client, Client)> {
    let logins = std::env::temp_dir().join(format!(
        "wamn-control-administration-{test}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&logins);
    std::fs::create_dir_all(&logins)?;
    let billing = project_database(
        admin,
        admin_url,
        admin_url,
        "billing",
        "aaaaaaa1",
        "t-billing",
        &logins,
    )
    .await?;
    let shop = project_database(
        admin, admin_url, admin_url, "shop", "aaaaaaa2", "t-shop", &logins,
    )
    .await?;
    Ok((logins, billing, shop))
}
