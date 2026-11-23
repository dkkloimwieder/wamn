//! Protect unfinished runs during application lifecycle changes.

use super::Platform;
use anyhow::Context as _;
use sqlx::FromRow as _;
use std::time::{Duration, Instant};
use wamn_registry::ApplicationName;
use wamn_registry::EnvName;

/// One application run that still needs a host.
#[derive(Debug)]
pub(super) struct UnfinishedRun {
    package_id: String,
    run_id: String,
}

#[derive(sqlx::FromRow)]
struct UnfinishedRunRow {
    package_id: String,
    run_id: String,
}

impl From<UnfinishedRunRow> for UnfinishedRun {
    fn from(row: UnfinishedRunRow) -> Self {
        Self {
            package_id: row.package_id,
            run_id: row.run_id,
        }
    }
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
    let runs = sqlx::query(
        "SELECT package_id, run_id FROM wamn_run.runs
          WHERE environment = $1 AND ($2::text IS NULL OR package_id = $2)
            AND status IN ('dispatched', 'running')
          ORDER BY package_id, run_id",
    )
    .bind(env.as_str())
    .bind(application.map(ApplicationName::as_str))
    .try_map(|row| UnfinishedRunRow::from_row(&row).map(UnfinishedRun::from))
    .fetch_all(&mut *transaction)
    .await
    .context("read unfinished application runs")?;
    transaction.commit().await?;
    Ok(runs)
}

/// Uninstall an application's release, and refuse while the application has unfinished runs.
pub(super) async fn stop(
    platform: &Platform,
    project: &mut sqlx::PgConnection,
    env: &EnvName,
    application: &ApplicationName,
    release: &str,
) -> anyhow::Result<()> {
    use wamn_kubernetes::helm::{self, Wait};
    let unfinished = unfinished(project, env, Some(application)).await?;
    anyhow::ensure!(unfinished.is_empty(), "{}", describe(&unfinished));
    let namespace = helm::org_release_namespace(&platform.target.namespace);
    helm::uninstall(&platform.target, &namespace, release, Wait::Watcher).await
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
