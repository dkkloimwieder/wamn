//! Step 8 of `apply`: drain (docs/plan/platform-deploy.md §10.1 step 8, R20
//! (2) to (5), §11.1).
//!
//! The old release's pods are terminating, and each finishes the runs pinned
//! to its release in its SIGTERM path. The verb waits for the live set to be
//! exactly the declared release, or empty for `release = none`, up to the
//! largest `terminationGracePeriodSeconds` the other pods carry. Kubernetes
//! owns that clock; the verb only observes it. Then a run still dispatched or
//! running whose release has no live pod is stranded: the verb names each run
//! and its digest and stops. Nothing fails a stranded run on its own.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use serde_json::Value;

use super::Platform;
use super::analyse::{Analysis, connect, live_set, read_pods};
use super::document::EnvironmentDocument;

/// How often the verb reads the live set.
const POLL: Duration = Duration::from_secs(2);

/// Time beyond the grace period for the kubelet to remove a killed pod.
const REMOVAL_SLACK: Duration = Duration::from_secs(15);

/// The stranded-run query: runs pinned to a release with no live pod.
const STRANDED_SQL: &str = "\
SELECT run_id, manifest_digest FROM wamn_run.runs \
 WHERE tenant_id = $1 AND environment = $2 \
   AND status IN ('dispatched', 'running') \
   AND manifest_digest IS NOT NULL \
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
    let tenant =
        wamn_control_provision::project_env_tenant(&document.org, &document.project, &document.env);
    let stranded = stranded(&project_url, &tenant, &document.env, &live).await?;
    if !stranded.is_empty() {
        let recovery = if target.is_some() {
            "`env rollback` brings hosts that can claim them"
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

/// Whether the live set is exactly the declared release, or empty for none.
pub fn settled(live: &BTreeMap<String, usize>, target: Option<&str>) -> bool {
    match target {
        None => live.is_empty(),
        Some(digest) => live.len() == 1 && live.contains_key(digest),
    }
}

/// The largest `terminationGracePeriodSeconds` of the pods that are not of
/// the declared release.
pub fn largest_grace(pods: &Value, target: Option<&str>) -> Duration {
    pods["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|pod| pod["metadata"]["annotations"]["wamn.release-digest"].as_str() != target)
        .filter_map(|pod| pod["spec"]["terminationGracePeriodSeconds"].as_u64())
        .max()
        .map_or(Duration::ZERO, Duration::from_secs)
}

/// The runs of the environment pinned to a release outside `live`.
///
/// # Errors
///
/// When the project database cannot be read.
pub async fn stranded(
    project_url: &str,
    tenant: &str,
    environment: &str,
    live: &BTreeMap<String, usize>,
) -> anyhow::Result<Vec<Stranded>> {
    let live: Vec<String> = live.keys().cloned().collect();
    let mut client = connect(project_url).await?;
    let transaction = client
        .transaction()
        .await
        .context("begin the stranded-run read")?;
    transaction
        .query_one("SELECT set_config('app.tenant', $1, true)", &[&tenant])
        .await
        .context("claim the tenant")?;
    let stranded = transaction
        .query(STRANDED_SQL, &[&tenant, &environment, &live])
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

/// "stranded runs: <run> (<digest>), ...".
pub fn describe(stranded: &[Stranded]) -> String {
    let mut text = String::from("stranded runs, pinned to a release with no live pod:");
    for run in stranded {
        write!(text, " {} ({})", run.run_id, run.manifest_digest)
            .expect("writing to a String succeeds");
    }
    text
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

    #[test]
    fn a_stranded_run_is_named_with_its_digest() {
        let text = describe(&[Stranded {
            run_id: "run-1".to_owned(),
            manifest_digest: "sha256:b".to_owned(),
        }]);
        assert!(text.ends_with(" run-1 (sha256:b)"), "{text}");
    }
}
