//! Environment-targeted logins on the shared control database.

use anyhow::Context as _;
use sqlx::Row as _;
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
    let base: wamn_postgres::ConnectionConfig = serde_json::from_str(admin_url)?;
    for family in WorkloadRoleFamily::project_env_families()
        .filter(|family| family.identity_type() == WorkloadRoleIdentityType::Control)
    {
        let username = workload_login(family, target)?;
        let present: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT FROM pg_roles WHERE rolname=$1)")
                .bind(&username)
                .fetch_one(&mut admin)
                .await?;
        if !present {
            continue;
        }
        let mut connection = base.clone();
        connection.username = username.clone();
        connection.credentials.name = wamn_control_provision::workload_secret_name(
            family,
            &triple.org,
            &triple.project,
            &triple.env,
        );
        let mut owner = connect(&connection.to_json()?).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "DROP OWNED BY {}",
            postgres_protocol::escape::escape_identifier(&username)
        )))
        .execute(&mut owner)
        .await?;
    }
    crate::provision_project_env::workload::remove_roles(
        &platform.system_database_url,
        &platform.target.kubeconfig,
        &platform.target.context,
        &triple.org,
        cluster,
        &names,
    )
    .await?;
    Ok(names)
}
