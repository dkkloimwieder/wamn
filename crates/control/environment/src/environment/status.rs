//! The status operation (docs/architecture/execution.md#host-run-routes,
//! wamn-snz0.6).
//!
//! Current state only, read and never written: the newest successful Helm
//! revision and its digest, the digest the host Deployment's pod template
//! carries and its rollout, the conditions of the release's
//! WorkloadDeployments, the live set, and the drain: the pods that are
//! terminating and the runs pinned to a release with no live pod. No history
//! of checks, no Secret, no verb. Who applied is in the Helm revision, read
//! with `helm history`.
//!
//! The Deployment, the WorkloadDeployments and the pods are read by
//! [`wamn_control_provision::environment_state`], the reader the control
//! host's `environment.status` also uses; this verb adds the Helm revision,
//! the terminating pods and the stranded runs.

pub(super) mod plan;
use plan::terminating;

use std::collections::BTreeMap;

use anyhow::Context as _;
use serde::Serialize;
use serde_json::Value;
use wamn_control_provision::environment_state::{environment_state, observe, release_digest};
use wamn_control_registry::Triple;

use super::Platform;
use super::analyse::read_revision;
use super::drain::{Stranded, stranded};
use super::ensure::{kubectl, project_url};
use wamn_package_control::database::connect;

pub use wamn_control_provision::environment_state::DeploymentStatus;

/// The status of one environment.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    pub applications: BTreeMap<String, ApplicationStatus>,
}

/// Independently observed state of one application's Helm release and host group.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApplicationStatus {
    pub release_name: String,
    /// The newest successful Helm revision, or none.
    pub revision: Option<RevisionStatus>,
    /// The host Deployment, or none.
    pub deployment: Option<DeploymentStatus>,
    /// Each WorkloadDeployment of the release, with its conditions.
    pub workloads: BTreeMap<String, BTreeMap<String, String>>,
    /// Release digest to the count of host pods that are not terminated.
    pub live: BTreeMap<String, usize>,
    /// Release digest to the count of host pods that are terminating.
    pub terminating: BTreeMap<String, usize>,
    /// Runs pinned to a release with no live pod.
    pub stranded: Vec<Stranded>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RevisionStatus {
    pub number: u32,
    pub chart: String,
    pub manifest_digest: Option<String>,
}

/// Read the status of one environment.
///
/// # Errors
///
/// When Helm, Kubernetes or the project database cannot be read.
pub async fn status(
    platform: &Platform,
    triple: &Triple,
    admin_url: &str,
) -> anyhow::Result<Status> {
    let mut revisions = BTreeMap::new();
    let mut memberships = wamn_catalog::EnvironmentApplications::new();
    for (application, release_name) in
        super::analyse::application_releases(platform, triple).await?
    {
        let revision = read_revision(platform, &release_name).await?;
        if let Some(revision) = &revision {
            memberships.extend(super::show::plan::revision_applications(&revision.values)?);
        }
        revisions.insert(application, (release_name, revision));
    }
    let mut applications = BTreeMap::new();
    for (application, (release_name, revision)) in revisions {
        let mut packages = vec![application.clone()];
        if let Some(membership) = memberships.get(&application) {
            packages.extend(
                membership
                    .packages
                    .iter()
                    .filter(|package| {
                        memberships
                            .values()
                            .filter(|owner| owner.packages.contains(*package))
                            .count()
                            == 1
                    })
                    .cloned(),
            );
        }
        let revision = revision.map(|revision| RevisionStatus {
            number: revision.number,
            chart: revision.chart.clone(),
            manifest_digest: revision.manifest_digest().map(str::to_owned),
        });
        let namespace = platform.target.namespace.as_str();
        let observed = observe(namespace, &release_name, |path| async move {
            let body = kubectl(platform, namespace, &["get", "--raw", &path], None).await?;
            serde_json::from_str::<Value>(&body).with_context(|| format!("decode {path}"))
        })
        .await?;
        let state = environment_state(&observed, &release_name);
        let stranded = stranded_runs(platform, triple, &packages, &state.live, admin_url).await?;
        applications.insert(
            application,
            ApplicationStatus {
                terminating: terminating(&observed.pods),
                release_name,
                revision,
                deployment: state.deployment,
                workloads: state.workloads,
                live: state.live,
                stranded,
            },
        );
    }
    Ok(Status { applications })
}

/// The runs pinned to a release with no live pod, or none when the
/// environment has no row or no database.
async fn stranded_runs(
    platform: &Platform,
    triple: &Triple,
    packages: &[String],
    live: &BTreeMap<String, usize>,
    admin_url: &str,
) -> anyhow::Result<Vec<Stranded>> {
    // A lifecycle verb: the provisioning credential, as the install Jobs use.
    let system = connect(admin_url).await?;
    system
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    let present = system
        .query_opt(
            "SELECT 1 FROM registry.project_envs WHERE org = $1 AND project = $2 AND env = $3",
            &[&triple.org, &triple.project, &triple.env.as_str()],
        )
        .await
        .context("read the environment row")?
        .is_some();
    if !present {
        return Ok(Vec::new());
    }
    let url = project_url(platform, triple).await?;
    if connect(&url).await.is_err() {
        return Ok(Vec::new());
    }
    let env_key =
        wamn_control_provision::project_env_key(&triple.org, &triple.project, triple.env.as_str());
    stranded(&url, &env_key, triple.env.as_str(), Some(packages), live).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_draining_pods_are_terminating() {
        let pods = json!({"items": [
            {"metadata": {"annotations": {"wamn.release-digest": "sha256:a"}, "deletionTimestamp": "t"}, "status": {"phase": "Running"}},
            {"metadata": {"annotations": {"wamn.release-digest": "sha256:b"}}, "status": {"phase": "Running"}}
        ]});
        assert_eq!(
            terminating(&pods),
            BTreeMap::from([("sha256:a".to_owned(), 1)])
        );
    }
}
