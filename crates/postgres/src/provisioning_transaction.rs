//! Provisioning transaction authority.
use crate::{PlatformComponent, bind_platform_principal};
use anyhow::Context as _;
use sqlx::{Connection as _, PgConnection as Client};
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
    bind_platform_principal(&mut transaction, PlatformComponent::Provisioning)
        .await
        .context("bind wamn:provisioning as the actor")?;
    Ok(transaction)
}
