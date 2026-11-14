use anyhow::Context as _;
use sqlx::Connection as _;

pub(super) async fn begin<'a>(
    client: &'a mut sqlx::PgConnection,
    project_env_key: &str,
) -> anyhow::Result<sqlx::Transaction<'a, sqlx::Postgres>> {
    let mut tx = client.begin().await.context("begin package SQL")?;
    sqlx::query(super::ASSIGN_PROJECT_ENV_KEY_SQL)
        .bind(project_env_key)
        .fetch_one(&mut *tx)
        .await?;
    super::bind_apply_package_principal(&mut tx).await?;
    Ok(tx)
}

/// A dedicated migration session keeps the caller binding across SQLx commits.
pub(super) async fn bind_session(
    connection: &mut sqlx::PgConnection,
    project_env_key: &str,
) -> anyhow::Result<()> {
    let component = wamn_control_provision::PlatformComponent::ApplyPackage;
    sqlx::query("SELECT set_config('app.project_env_key', $1, false), set_config('app.user_id', $2, false), set_config('app.operation', $3, false)")
        .bind(project_env_key).bind(component.principal_id().to_string()).bind(component.principal_name())
        .execute(connection).await?;
    Ok(())
}
