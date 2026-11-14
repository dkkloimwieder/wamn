//! Observe resident application releases and the runs that pin them.

use super::{Platform, analyse::Analysis, document::EnvironmentDocument};
use anyhow::{Context as _, bail};
use sqlx::{Connection as _, Row as _};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
pub use wamn_kubernetes::environment_state::ReleaseResidency;

/// Describe runs whose release is not loaded by any environment host.
pub fn describe(stranded: &[Stranded]) -> String {
    use std::fmt::Write as _;
    let mut text = String::from("stranded runs:");
    for run in stranded {
        write!(text, " {} ({})", run.run_id, run.manifest_digest)
            .expect("writing to a String succeeds");
    }
    text
}

/// Read actual host residency through the Kubernetes pod proxy.
pub async fn observe(
    platform: &Platform,
    triple: &wamn_registry::Triple,
) -> anyhow::Result<Vec<ReleaseResidency>> {
    let reports = wamn_kubernetes::environment_state::residency(
        &platform.target.namespace,
        &triple.org,
        triple.env.as_str(),
        |path| async move {
            let body = super::ensure::kubectl(
                platform,
                &platform.target.namespace,
                &["get", "--raw", &path],
                None,
            )
            .await?;
            serde_json::from_str(&body).context("read environment residency")
        },
    )
    .await?;
    wamn_kubernetes::environment_state::project_residency(reports, &triple.project)
}

/// Observe all releases together. A failed release never implies another release unloaded.
pub async fn drain(
    platform: &Platform,
    document: &EnvironmentDocument,
    _analysis: &Analysis,
) -> anyhow::Result<Vec<ReleaseResidency>> {
    let residency = observe(platform, &document.triple()).await?;
    let live = loaded(&residency);
    let project_url = super::ensure::project_url(platform, &document.triple()).await?;
    let key = wamn_registry::project_env_key(&document.org, &document.project, &document.env);
    let stranded = stranded(&project_url, &key, &document.env, None, &live).await?;
    if !stranded.is_empty() {
        bail!(
            "{}; restore their releases to resume them",
            describe(&stranded)
        );
    }
    Ok(residency)
}

/// Loaded application and digest membership for the database's unfinished-run boundary.
pub fn loaded(residency: &[ReleaseResidency]) -> BTreeMap<(String, String), usize> {
    let mut live = BTreeMap::new();
    for release in residency.iter().filter(|release| release.loaded) {
        *live
            .entry((release.application.clone(), release.manifest_digest.clone()))
            .or_default() += 1;
    }
    live
}

/// Wait only for a stopped host Deployment's pods to terminate.
/// Release residency is independent of this Kubernetes shutdown bound.
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

/// The stranded-run query: runs pinned to a release with no loaded release.
const STRANDED_SQL: &str = "\
SELECT run_id, manifest_digest FROM wamn_run.runs \
 WHERE project_env_key = $1 AND environment = $2 \
   AND status NOT IN ('completed', 'failed', 'infrastructure-failure') \
   AND manifest_digest IS NOT NULL \
   AND ($4::text[] IS NULL OR package_id = ANY($4)) \
   AND NOT EXISTS (SELECT FROM unnest($3::text[], $5::text[]) \
       AS loaded(application, digest) WHERE loaded.application = runs.package_id \
       AND loaded.digest = runs.manifest_digest) \
 ORDER BY manifest_digest, run_id";

/// One run pinned to a release with no loaded release.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Stranded {
    pub run_id: String,
    pub manifest_digest: String,
}

/// The runs of the environment pinned to a release outside `live`.
///
/// # Errors
///
/// When the project database cannot be read.
pub async fn stranded(
    project_url: &str,
    project_env_key: &str,
    environment: &str,
    applications: Option<&[String]>,
    live: &BTreeMap<(String, String), usize>,
) -> anyhow::Result<Vec<Stranded>> {
    let (loaded_applications, loaded_digests): (Vec<&str>, Vec<&str>) = live
        .keys()
        .map(|(application, digest)| (application.as_str(), digest.as_str()))
        .unzip();
    let mut client = super::analyse::connect_project(project_url).await?;
    let mut transaction = client
        .begin()
        .await
        .context("begin the stranded-run read")?;
    sqlx::query("SELECT set_config('app.project_env_key', $1, true)")
        .bind(project_env_key)
        .fetch_one(&mut *transaction)
        .await
        .context("bind the project identity")?;
    let stranded = sqlx::query(STRANDED_SQL)
        .bind(project_env_key)
        .bind(environment)
        .bind(&loaded_applications)
        .bind(applications)
        .bind(&loaded_digests)
        .fetch_all(&mut *transaction)
        .await
        .context("read the runs pinned to a release with no loaded release")?
        .into_iter()
        .map(|row| Stranded {
            run_id: row.get(0),
            manifest_digest: row.get(1),
        })
        .collect();
    transaction
        .commit()
        .await
        .context("end the stranded-run read")?;
    Ok(stranded)
}
