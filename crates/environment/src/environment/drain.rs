//! Protect unfinished runs during application lifecycle changes.

use super::Platform;
use anyhow::Context as _;
use std::time::{Duration, Instant};
use wamn_registry::ApplicationName;
use wamn_registry::EnvName;

/// One application run that still needs a host.
#[derive(Debug, sqlx::FromRow)]
pub(super) struct UnfinishedRun {
    package_id: String,
    run_id: String,
}

pub(super) fn describe(unfinished: &[UnfinishedRun]) -> String {
    use std::fmt::Write as _;
    let mut text = String::from("unfinished workflow runs:");
    for run in unfinished {
        write!(text, " {}/{}", run.package_id, run.run_id).expect("writing to a String succeeds");
    }
    text
}

pub(super) async fn unfinished(
    client: &mut sqlx::PgConnection,
    env: &EnvName,
    application: Option<&ApplicationName>,
) -> anyhow::Result<Vec<UnfinishedRun>> {
    let mut transaction = crate::provision_project_env::provisioning_transaction(client).await?;
    let runs = sqlx::query_as(
        "SELECT package_id, run_id FROM wamn_run.runs
          WHERE environment = $1 AND ($2::text IS NULL OR package_id = $2)
            AND status IN ('dispatched', 'running')
          ORDER BY package_id, run_id",
    )
    .bind(env)
    .bind(application)
    .fetch_all(&mut *transaction)
    .await
    .context("read unfinished application runs")?;
    transaction.commit().await?;
    Ok(runs)
}

/// Wait only for a stopped host Deployment's pods to terminate.
pub async fn wait_stopped(platform: &Platform, name: &str) -> anyhow::Result<()> {
    let started = Instant::now();
    let mut deadline = started + Duration::from_secs(15);
    loop {
        let pods = super::analyse::read_pods(platform, name).await?;
        let live: Vec<_> = pods["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|pod| {
                !matches!(
                    pod["status"]["phase"].as_str(),
                    Some("Succeeded" | "Failed")
                )
            })
            .collect();
        if live.is_empty() {
            return Ok(());
        }
        let grace = live
            .iter()
            .filter_map(|pod| pod["spec"]["terminationGracePeriodSeconds"].as_u64())
            .max()
            .unwrap_or(0);
        deadline = deadline.max(started + Duration::from_secs(grace.saturating_add(15)));
        anyhow::ensure!(
            Instant::now() < deadline,
            "environment host {name} did not stop within its shutdown bound"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
