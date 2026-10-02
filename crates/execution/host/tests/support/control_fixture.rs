//! The fixture of the control host route tests: the system schema, one org
//! with two projects and one environment each, and the org's `control`
//! login (docs/plan/platform-ui.md §4.4 and §4.5).
//!
//! `control_route_live` and `tests/integration`'s `control_client_live`
//! include it.

use tokio_postgres::{Client, NoTls};
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
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
