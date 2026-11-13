//! Reading the org-targeted `registry.env_policies` rows from the T1 system DB
//! (D18; org-targeted by wamn-8df.4).
//!
//! `provision-org` (sizes each cluster from the org's policy set) and
//! `provision-project-env` (reads one of the org's policies to derive the
//! project-env's cluster owner) map `select_env_policies_sql` /
//! `select_env_policy_sql` rows into the pure [`EnvPolicy`] model here.
//! `recovery_domain` is `jsonb` selected as `text` (column index 1), parsed back
//! into [`RecoveryDomain`] via serde.

use anyhow::Context as _;
use wamn_control_provision::EnvPolicy;
use wamn_release_control::env_policies::env_policy_from_row;

/// Read an org's whole env-policy set from the system DB, ordered by
/// `promotion_rank`. Empty for an org with no stamped policies yet.
pub async fn read_env_policies(
    client: &tokio_postgres::Client,
    org: &str,
) -> anyhow::Result<Vec<EnvPolicy>> {
    let rows = client
        .query(
            wamn_registry::sql::select_env_policies_sql().as_str(),
            &[&org],
        )
        .await
        .context("select env_policies")?;
    rows.iter().map(env_policy_from_row).collect()
}
