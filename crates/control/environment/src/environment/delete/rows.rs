//! The final delete transaction retains retry inputs until credential cleanup ends.

use anyhow::Context as _;
use wamn_control_registry::Triple;

/// Remove the environment's registry rows last.
/// The caller holds the lifecycle lock and removes external resources first.
///
/// # Errors
///
/// Returns an error if a database operation or row removal fails.
pub async fn delete_registry_rows(
    system: &mut tokio_postgres::Client,
    triple: &Triple,
    instance: &str,
) -> anyhow::Result<Vec<String>> {
    let (org, project, env) = (
        triple.org.as_str(),
        triple.project.as_str(),
        triple.env.as_str(),
    );
    let env_key = wamn_control_provision::project_env_key(org, project, env);
    let mut lines = Vec::new();
    let transaction = crate::provision_project_env::provisioning_transaction(system)
        .await
        .context("begin the row delete")?;
    transaction
        .query_one("SELECT set_config('app.env_key', $1, true)", &[&env_key])
        .await
        .context("claim the tenant")?;
    let credentials: i64 = transaction
        .query_one(
            wamn_control_registry::sql::delete_environment_connections_sql(),
            &[&org, &project, &env],
        )
        .await
        .context("delete the environment's connections")?
        .get(0);
    let env_key_table: bool = transaction
        .query_one(
            "SELECT to_regclass('catalog.tenant_environments') IS NOT NULL",
            &[],
        )
        .await
        .context("probe catalog.tenant_environments")?
        .get(0);
    if env_key_table {
        transaction
            .execute(
                "DELETE FROM catalog.tenant_environments \
                  WHERE org = $1 AND project = $2 AND env = $3 AND instance_suffix = $4",
                &[&org, &project, &env, &instance],
            )
            .await
            .context("delete the catalog.tenant_environments row")?;
    }
    transaction
        .execute(
            "DELETE FROM wamn_authority.author_login_tenants \
              WHERE org_id=$1 AND project_id=$2 AND environment=$3",
            &[&org, &project, &env],
        )
        .await
        .context("delete the environment's control login mappings")?;
    transaction
        .execute(
            "DELETE FROM registry.project_envs WHERE org = $1 AND project = $2 AND env = $3",
            &[&org, &project, &env],
        )
        .await
        .context("delete the registry.project_envs row")?;
    transaction
        .commit()
        .await
        .context("commit the row delete")?;
    lines.push(format!(
        "deleted {credentials} connection credentials and the environment row of {triple}"
    ));
    Ok(lines)
}
