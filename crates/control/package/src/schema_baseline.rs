//! Record and observe initialization digests.

pub(super) mod plan;
use plan::compare_digest;

use anyhow::Context as _;
use tokio_postgres::GenericClient;
use wamn_control_provision::schema_baseline::{project_sha256, system_sha256};

/// Record the system initialization schema in the caller's current transaction or session.
pub async fn record_system(client: &impl GenericClient) -> anyhow::Result<()> {
    record(client, "registry.schema_baseline", &system_sha256()).await
}

/// Record the project initialization schema in the caller's current transaction or session.
pub async fn record_project(client: &impl GenericClient) -> anyhow::Result<()> {
    record(client, "platform.schema_baseline", &project_sha256()).await
}

/// Record the digest after initialization.
async fn record(client: &impl GenericClient, table: &str, sha256: &str) -> anyhow::Result<()> {
    client
        .execute(
            &format!("INSERT INTO {table} (sha256) VALUES ($1)"),
            &[&sha256],
        )
        .await
        .with_context(|| format!("record {table}"))?;
    Ok(())
}

/// Observe the system baseline without authorizing schema writes.
pub async fn observe_system(
    client: &impl GenericClient,
) -> anyhow::Result<Option<BaselineDifference>> {
    compare(client, "registry.schema_baseline", &system_sha256()).await
}

/// Observe the project baseline without authorizing schema writes.
pub async fn observe_project(
    client: &impl GenericClient,
) -> anyhow::Result<Option<BaselineDifference>> {
    compare(client, "platform.schema_baseline", &project_sha256()).await
}

async fn compare(
    client: &impl GenericClient,
    table: &str,
    candidate: &str,
) -> anyhow::Result<Option<BaselineDifference>> {
    let exists: bool = client
        .query_one("SELECT to_regclass($1) IS NOT NULL", &[&table])
        .await
        .with_context(|| format!("look for {table}"))?
        .get(0);
    let installed = if exists {
        client
            .query_opt(&format!("SELECT sha256 FROM {table}"), &[])
            .await
            .with_context(|| format!("read {table}"))?
            .map(|row| row.get::<_, String>(0))
    } else {
        None
    };
    Ok(compare_digest(installed.as_deref(), candidate))
}

/// Recorded and selected schema identities. Absence is an observation, not permission to initialize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineDifference {
    pub installed: Option<String>,
    pub candidate: String,
}

#[cfg(test)]
mod tests {
    use super::compare_digest;

    #[test]
    fn changed_or_absent_baselines_are_observations() {
        let candidate = "sha256:selected";
        for installed in [Some("sha256:installed"), None] {
            let difference = compare_digest(installed, candidate).unwrap();
            assert_eq!(difference.installed.as_deref(), installed);
            assert_eq!(difference.candidate, candidate);
        }
        assert!(compare_digest(Some(candidate), candidate).is_none());
    }
}
