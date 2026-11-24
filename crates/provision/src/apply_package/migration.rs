use anyhow::Context as _;
use sqlx::Connection as _;

pub(super) async fn begin<'a>(
    client: &'a mut sqlx::PgConnection,
    _project_env_key: &wamn_registry::ProjectEnvKey,
) -> anyhow::Result<sqlx::Transaction<'a, sqlx::Postgres>> {
    let mut tx = client.begin().await.context("begin package SQL")?;
    super::bind_apply_package_principal(&mut tx).await?;
    Ok(tx)
}
