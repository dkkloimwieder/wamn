//! Environment-targeted logins on the shared control database.

use anyhow::Context as _;
use postgres_protocol::escape::escape_identifier;
use sqlx::{Connection as _, Row as _};
use wamn_control_provision::{
    WorkloadRoleFamily, WorkloadRoleIdentity, WorkloadRoleIdentityType, workload_login,
};
use wamn_postgres::connect;
use wamn_registry::Triple;

pub(super) async fn delete_control_roles(
    admin_url: &str,
    triple: &Triple,
    platform: &crate::environment::Platform,
) -> anyhow::Result<Vec<String>> {
    let mut admin = connect(admin_url).await?;
    let database: String = sqlx::query("SELECT current_database()")
        .fetch_one(&mut admin)
        .await?
        .get(0);
    let target = WorkloadRoleIdentity::Control {
        org: &triple.org,
        project: &triple.project,
        environment: &triple.env,
        database: &database,
    };
    let options = wamn_postgres::connect_options(admin_url).await?;
    let cluster = options
        .get_host()
        .split('.')
        .next()
        .context("control database host")?
        .trim_end_matches("-rw");
    let names = WorkloadRoleFamily::project_env_families()
        .filter(|family| family.identity_type() == WorkloadRoleIdentityType::Control)
        .map(|family| workload_login(family, target))
        .collect::<Result<Vec<_>, _>>()?;
    crate::provision_project_env::workload::remove_roles(
        &platform.target.kubeconfig,
        &platform.target.context,
        &triple.org,
        cluster,
        &names,
    )
    .await?;
    let mut roles = Vec::new();
    for family in WorkloadRoleFamily::project_env_families()
        .filter(|family| family.identity_type() == WorkloadRoleIdentityType::Control)
    {
        let role = workload_login(family, target)?;
        if sqlx::query("SELECT 1 FROM pg_roles WHERE rolname=$1")
            .bind(&role)
            .fetch_optional(&mut admin)
            .await?
            .is_some()
        {
            let mut transaction = admin.begin().await?;
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "DROP OWNED BY {role}; DROP ROLE {role}",
                role = escape_identifier(&role)
            )))
            .execute(&mut *transaction)
            .await
            .map(drop)
            .with_context(|| format!("drop environment control role {role}"))?;
            transaction.commit().await?;
        }
        roles.push(role);
    }
    Ok(roles)
}
