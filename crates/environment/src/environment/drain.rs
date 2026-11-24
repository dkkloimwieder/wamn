//! Protect unfinished runs during application lifecycle changes.

use super::Platform;
use std::time::{Duration, Instant};
use wamn_registry::ApplicationName;
use wamn_registry::EnvName;

/// One application run that still needs a host.
#[derive(Debug)]
pub struct UnfinishedRun {
    pub package_id: String,
    pub run_id: String,
}

pub(super) fn describe(unfinished: &[UnfinishedRun]) -> String {
    use std::fmt::Write as _;
    let mut text = String::from("unfinished workflow runs:");
    for run in unfinished {
        write!(text, " {}/{}", run.package_id, run.run_id).expect("writing to a String succeeds");
    }
    text
}

/// Uninstall an application's release, and refuse while the application has unfinished runs.
pub(super) async fn stop(
    platform: &Platform,
    project: &crate::environment_sql_effects::PostgresSession,
    env: &EnvName,
    application: &ApplicationName,
    release: &str,
) -> anyhow::Result<()> {
    let unfinished = project.unfinished(env, Some(application)).await?;
    anyhow::ensure!(unfinished.is_empty(), "{}", describe(&unfinished));
    platform.cluster.uninstall(release).await
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
