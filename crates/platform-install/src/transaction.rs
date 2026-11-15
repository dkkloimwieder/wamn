//! Provisioning transaction authority.
use anyhow::Context as _;
use sqlx::{Connection as _, PgConnection as Client};
use wamn_control_provision::{PlatformComponent, bind_platform_principal_sql};
/// Begin a system database transaction that writes as `wamn:provisioning`.
///
/// The identity relations stamp the bound actor. The binding ends with the
/// transaction.
pub async fn provisioning_transaction(
    client: &mut Client,
) -> anyhow::Result<sqlx::Transaction<'_, sqlx::Postgres>> {
    let mut transaction = client
        .begin()
        .await
        .context("begin a system database write transaction")?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(bind_platform_principal_sql(
        PlatformComponent::Provisioning,
    )))
    .execute(&mut *transaction)
    .await
    .map(drop)
    .context("bind wamn:provisioning as the actor")?;
    Ok(transaction)
}
