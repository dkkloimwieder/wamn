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
use sqlx::Row as _;
use sqlx::postgres::PgRow as Row;
use wamn_control_provision::{EnvPolicy, RecoveryDomain};
use wamn_registry::Env;

/// Map one `select_env_policies_sql` / `select_env_policy_sql` row into an
/// [`EnvPolicy`]. Column order: `name, recovery_domain::text, promotion_rank,
/// instances, storage, cpu, memory, image, backup_cadence, wal_retention,
/// hibernation`.
pub fn env_policy_from_row(row: &Row) -> anyhow::Result<EnvPolicy> {
    let recovery_text: String = row.get(1);
    let recovery_domain: RecoveryDomain =
        serde_json::from_str(&recovery_text).context("parse recovery_domain jsonb")?;
    Ok(EnvPolicy {
        name: Env::new(row.get::<String, _>(0)),
        recovery_domain,
        promotion_rank: row.get(2),
        instances: row.get(3),
        storage: row.get(4),
        cpu: row.get(5),
        memory: row.get(6),
        image: row.get(7),
        backup_cadence: row.get(8),
        wal_retention: row.get(9),
        hibernation: row.get(10),
    })
}

/// Read one env policy from an org's set, or `None` if the slug names none of
/// the org's policies.
pub async fn read_env_policy(
    client: &mut sqlx::PgConnection,
    org: &str,
    name: &str,
) -> anyhow::Result<Option<EnvPolicy>> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(
        (wamn_registry::sql::select_env_policy_sql().as_str()).to_string(),
    ))
    .bind(org)
    .bind(name)
    .fetch_all(&mut *client)
    .await
    .context("select env_policy")?;
    rows.first().map(env_policy_from_row).transpose()
}
