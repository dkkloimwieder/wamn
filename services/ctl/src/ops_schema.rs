//! Operations-state installation and least-privilege session entry.

use anyhow::Context as _;

/// Install the idempotent operations artifact as its owner, then constrain the
/// connection to the stable operations role before any state read or write.
pub(crate) async fn install_and_enter(client: &tokio_postgres::Client) -> anyhow::Result<()> {
    client
        .batch_execute("RESET ROLE")
        .await
        .context("RESET ROLE before operations-state install")?;
    client
        .batch_execute(wamn_control_provision::state::ensure_ops_role_sql())
        .await
        .context("create or harden wamn_ops")?;
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system for operations-state install")?;
    client
        .batch_execute(wamn_control_provision::OPS_SCHEMA_SQL)
        .await
        .context("install operations-state schema")?;
    client
        .batch_execute("RESET ROLE; SET ROLE wamn_ops")
        .await
        .context("SET ROLE wamn_ops")?;
    Ok(())
}
