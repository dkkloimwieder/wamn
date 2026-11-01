//! Record full-schema digests and refuse a changed installation.

use std::fmt;

use anyhow::Context as _;
use tokio_postgres::GenericClient;
use wamn_control_provision::schema_baseline::{project_sha256, system_sha256};

use crate::environment::analyse::connect;

/// Record the full system schema in the caller's current transaction or session.
pub(crate) async fn record_system(client: &impl GenericClient) -> anyhow::Result<()> {
    record(client, "registry.schema_baseline", &system_sha256()).await
}

/// Record the full project schema in the caller's current transaction or session.
pub(crate) async fn record_project(client: &impl GenericClient) -> anyhow::Result<()> {
    record(client, "platform.schema_baseline", &project_sha256()).await
}

/// Record the digest after full-schema installation.
///
/// Migrations are deferred until development is stable, not removed.
/// They return then with a smaller design.
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

/// Refuse a missing or changed system baseline before environment analysis.
pub(crate) async fn compare_system(system_url: &str) -> anyhow::Result<()> {
    compare(
        &connect(system_url).await?,
        "registry.schema_baseline",
        &system_sha256(),
    )
    .await
}

/// Refuse a missing or changed project baseline before package application.
pub(crate) async fn compare_project(project_url: &str) -> anyhow::Result<()> {
    compare(
        &connect(project_url).await?,
        "platform.schema_baseline",
        &project_sha256(),
    )
    .await
}

/// Compare an installed project baseline before reconciliation and skip a fresh database.
pub(crate) async fn compare_project_if_installed(project_url: &str) -> anyhow::Result<()> {
    let installed: bool = connect(project_url)
        .await?
        .query_one("SELECT to_regclass('platform.\"user\"') IS NOT NULL", &[])
        .await
        .context("look for the installed project schema")?
        .get(0);
    if installed {
        compare_project(project_url).await?;
    }
    Ok(())
}

async fn compare(client: &impl GenericClient, table: &str, candidate: &str) -> anyhow::Result<()> {
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
    compare_digest(installed.as_deref(), candidate)?;
    Ok(())
}

#[derive(Debug)]
struct BaselineMismatch {
    installed: Option<String>,
    candidate: String,
}

impl fmt::Display for BaselineMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "schema changed, reset the environment database (installed: {:?}, candidate: {})",
            self.installed, self.candidate
        )
    }
}

impl std::error::Error for BaselineMismatch {}

fn compare_digest(installed: Option<&str>, candidate: &str) -> Result<(), BaselineMismatch> {
    if installed == Some(candidate) {
        Ok(())
    } else {
        Err(BaselineMismatch {
            installed: installed.map(str::to_owned),
            candidate: candidate.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::compare_digest;

    #[test]
    fn changed_or_absent_baseline_refuses_apply() {
        let candidate = format!("sha256:{}", "a".repeat(64));
        let changed = format!("sha256:{}", "b".repeat(64));
        for installed in [Some(changed.as_str()), None] {
            let error = compare_digest(installed, &candidate).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("schema changed, reset the environment database")
            );
        }
        assert!(compare_digest(Some(&candidate), &candidate).is_ok());
    }
}
