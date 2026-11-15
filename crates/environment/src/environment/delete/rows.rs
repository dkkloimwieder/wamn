//! The final delete transaction retains retry inputs until credential cleanup ends.

use anyhow::Context as _;
use sqlx::Row as _;
use wamn_registry::Triple;

/// Remove the environment's registry rows last.
/// The caller holds the lifecycle lock and removes external resources first.
///
/// # Errors
///
/// Returns an error if a database operation or row removal fails.
pub async fn delete_registry_rows(
    system: &mut sqlx::PgConnection,
    triple: &Triple,
    _instance: &str,
) -> anyhow::Result<Vec<String>> {
    let (org, project, env) = (
        triple.org.as_str(),
        triple.project.as_str(),
        triple.env.as_str(),
    );
    let project_env_key = wamn_control_provision::project_env_key(org, project, env);
    let mut lines = Vec::new();
    let mut transaction = crate::provision_project_env::provisioning_transaction(system)
        .await
        .context("begin the row delete")?;
    sqlx::query("SELECT set_config('app.project_env_key', $1, true)")
        .bind(&project_env_key)
        .fetch_one(&mut *transaction)
        .await
        .context("bind the project identity")?;
    let credentials: i64 = sqlx::query(sqlx::AssertSqlSafe(
        (wamn_registry::sql::delete_environment_connections_sql()).to_string(),
    ))
    .bind(org)
    .bind(project)
    .bind(env)
    .fetch_one(&mut *transaction)
    .await
    .context("delete the environment's connections")?
    .get(0);
    sqlx::query(
        "DELETE FROM wamn_authority.author_login_projects \
              WHERE org_id=$1 AND project_id=$2 AND environment=$3",
    )
    .bind(org)
    .bind(project)
    .bind(env)
    .execute(&mut *transaction)
    .await
    .map(|result| result.rows_affected())
    .context("delete the environment's control login mappings")?;
    sqlx::query(
        wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_DELETE_ROWS_DELETE_REGISTRY_ROWS,
    )
    .bind(org)
    .bind(project)
    .bind(env)
    .execute(&mut *transaction)
    .await
    .map(|result| result.rows_affected())
    .context("delete the registry.environment_projects row")?;
    transaction
        .commit()
        .await
        .context("commit the row delete")?;
    lines.push(format!(
        "deleted {credentials} connection credentials and the environment row of {triple}"
    ));
    Ok(lines)
}
