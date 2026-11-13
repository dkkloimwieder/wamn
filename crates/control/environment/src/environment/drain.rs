//! Step 8 of `apply`: drain
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! The old release's pods are terminating, and each finishes the runs pinned
//! to its release in its SIGTERM path. The verb waits for the live set to be
//! exactly the declared release, or empty for `release = none`, up to the
//! largest `terminationGracePeriodSeconds` the other pods carry. Kubernetes
//! owns that clock; the verb only observes it. Then a run still dispatched or
//! running whose release has no live pod is stranded: the verb names each run
//! and its digest and stops. Nothing fails a stranded run on its own.

pub(super) mod plan;
pub use plan::{describe, largest_grace, settled};

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use serde_json::Value;

use super::Platform;
use super::analyse::{Analysis, live_set, read_pods};
use super::document::EnvironmentDocument;

/// How often the verb reads the live set.
const POLL: Duration = Duration::from_secs(2);

/// Time beyond the grace period for the kubelet to remove a killed pod.
const REMOVAL_SLACK: Duration = Duration::from_secs(15);

/// The stranded-run query: runs pinned to a release with no live pod.
const STRANDED_SQL: &str = "\
SELECT run_id, manifest_digest FROM wamn_run.runs \
 WHERE project_env_key = $1 AND environment = $2 \
   AND status NOT IN ('completed', 'failed', 'infrastructure-failure') \
   AND manifest_digest IS NOT NULL \
   AND ($4::text[] IS NULL OR package_id = ANY($4)) \
   AND NOT (manifest_digest = ANY($3)) \
 ORDER BY manifest_digest, run_id";

/// One run pinned to a release with no live pod.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Stranded {
    pub run_id: String,
    pub manifest_digest: String,
}

/// Step 8: wait for the live set, then refuse on stranded runs. Returns the
/// live set.
///
/// # Errors
///
/// When pods of another release are still live after the drain bound, or a
/// run is stranded. The error names each run and its digest, and the way back.
pub async fn drain(
    platform: &Platform,
    document: &EnvironmentDocument,
    analysis: &Analysis,
) -> anyhow::Result<BTreeMap<String, usize>> {
    let target = analysis
        .authorities
        .release
        .as_ref()
        .map(|release| release.digest.as_str());
    let live = wait(platform, &analysis.authorities.release_name, target).await?;
    if !settled(&live, target) {
        bail!(
            "the live set is {:?} after the drain bound; it should be {}",
            live,
            target.unwrap_or("empty")
        );
    }
    let project_url = super::ensure::project_url(platform, &document.triple()).await?;
    let project_env_key =
        wamn_control_provision::project_env_key(&document.org, &document.project, &document.env);
    let membership = &document.applications[&analysis.application];
    let mut packages: Vec<_> = membership
        .packages
        .iter()
        .filter(|package| {
            document.application_for_package(package) == Some(analysis.application.as_str())
        })
        .cloned()
        .collect();
    packages.push(analysis.application.clone());
    let stranded = stranded(
        &project_url,
        &project_env_key,
        &document.env,
        Some(&packages),
        &live,
    )
    .await?;
    if !stranded.is_empty() {
        let recovery = if target.is_some() {
            "`env rollback` brings hosts that can lease them"
        } else {
            "apply a document that names their release again"
        };
        bail!("{}; {recovery}", describe(&stranded));
    }
    Ok(live)
}

/// Wait until the live set is exactly `target`, or empty without one, up to
/// the largest grace period of the other pods. Returns the last live set read.
///
/// # Errors
///
/// When the pods cannot be read.
pub async fn wait(
    platform: &Platform,
    release_name: &str,
    target: Option<&str>,
) -> anyhow::Result<BTreeMap<String, usize>> {
    let started = Instant::now();
    let mut deadline = started + REMOVAL_SLACK;
    loop {
        let pods = read_pods(platform, release_name).await?;
        let live = live_set(&pods);
        if settled(&live, target) {
            return Ok(live);
        }
        deadline = deadline.max(started + largest_grace(&pods, target) + REMOVAL_SLACK);
        if Instant::now() >= deadline {
            return Ok(live);
        }
        tokio::time::sleep(POLL).await;
    }
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
    live: &BTreeMap<String, usize>,
) -> anyhow::Result<Vec<Stranded>> {
    let live: Vec<String> = live.keys().cloned().collect();
    let mut client = super::analyse::connect_project(project_url).await?;
    let transaction = client
        .transaction()
        .await
        .context("begin the stranded-run read")?;
    transaction
        .query_one(
            "SELECT set_config('app.project_env_key', $1, true)",
            &[&project_env_key],
        )
        .await
        .context("bind the project identity")?;
    let stranded = transaction
        .query(
            STRANDED_SQL,
            &[&project_env_key, &environment, &live, &applications],
        )
        .await
        .context("read the runs pinned to a release with no live pod")?
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_live_set_settles_on_exactly_the_declared_release() {
        let one = BTreeMap::from([("sha256:a".to_owned(), 2)]);
        let two = BTreeMap::from([("sha256:a".to_owned(), 2), ("sha256:b".to_owned(), 1)]);
        assert!(settled(&one, Some("sha256:a")));
        assert!(!settled(&two, Some("sha256:a")));
        assert!(!settled(&one, Some("sha256:b")));
        assert!(!settled(&BTreeMap::new(), Some("sha256:a")));
        assert!(settled(&BTreeMap::new(), None));
        assert!(!settled(&one, None));
    }

    #[test]
    fn the_bound_is_the_largest_grace_of_the_other_pods() {
        let pods = json!({"items": [
            {"metadata": {"annotations": {"wamn.release-digest": "sha256:a"}}, "spec": {"terminationGracePeriodSeconds": 900}},
            {"metadata": {"annotations": {"wamn.release-digest": "sha256:b"}}, "spec": {"terminationGracePeriodSeconds": 300}},
            {"metadata": {"annotations": {"wamn.release-digest": "sha256:c"}}, "spec": {"terminationGracePeriodSeconds": 120}}
        ]});
        assert_eq!(
            largest_grace(&pods, Some("sha256:a")),
            Duration::from_mins(5)
        );
        assert_eq!(largest_grace(&pods, None), Duration::from_mins(15));
        assert_eq!(largest_grace(&json!({"items": []}), None), Duration::ZERO);
    }
}
