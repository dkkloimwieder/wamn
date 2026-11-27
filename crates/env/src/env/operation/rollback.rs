//! Resolve an explicit Helm rollback target under the worker's lifecycle lock.

use std::{num::NonZeroU32, time::Duration};

use anyhow::{Context as _, ensure};
use serde::Deserialize as _;
use serde_json::Value;
use tokio::time::Instant;
use wamn_app::app_config::{APP_CONFIG, AppConfig};
use wamn_digest::Digest;
use wamn_kubernetes::helm::require_app_service_route;
use wamn_registry::AppName;
use wamn_registry::Triple;

use super::Platform;

pub(super) async fn target(
    platform: &Platform,
    triple: &Triple,
    app: &AppName,
    release: &str,
    revision: Option<NonZeroU32>,
) -> anyhow::Result<u32> {
    if let Some(revision) = revision {
        let revision = revision.get();
        let manifest = platform
            .cluster
            .release_manifest(release, revision)
            .await?
            .with_context(|| format!("Helm revision {revision} is absent"))?;
        require_app_service_route(&manifest, &platform.cluster.namespace, release)?;
        return Ok(revision);
    }
    let live = crate::env::document::workload_release(&platform.cluster, triple, app)
        .await?
        .context("application has no deployed release")?
        .revision;
    let name = crate::app_chart::plan::workload_name(release);
    for candidate in (1..live).rev() {
        let Some(manifest) = platform
            .cluster
            .release_manifest(release, candidate)
            .await?
        else {
            continue;
        };
        for document in serde_yaml::Deserializer::from_slice(&manifest) {
            let workload = Value::deserialize(document)?;
            if workload["kind"] == "WorkloadDeployment" && workload["metadata"]["name"] == name {
                let revision = workload["metadata"]["annotations"]["wamn.release-revision"]
                    .as_str()
                    .context("rollback candidate has no content revision")?
                    .parse::<u32>()?;
                if revision == candidate {
                    require_app_service_route(&manifest, &platform.cluster.namespace, release)?;
                    return Ok(candidate);
                }
                break;
            }
        }
    }
    anyhow::bail!("live content revision {live} has nothing older to roll back to")
}

/// Read the target's package pin and the application configuration Helm will restore.
pub(super) async fn app(
    platform: &Platform,
    release: &str,
    revision: u32,
) -> anyhow::Result<(Digest, AppConfig)> {
    let values = platform
        .cluster
        .release_values(release, revision)
        .await?
        .with_context(|| format!("Helm revision {revision} is absent"))?;
    let values: Value = serde_json::from_slice(&values)?;
    let digest = values["release"]["manifestDigest"]
        .as_str()
        .context("rollback revision has no package digest")?
        .parse()?;
    let app = values["route"]["config"][APP_CONFIG]
        .as_str()
        .context("rollback revision has no wamn.app configuration")?;
    Ok((digest, serde_json::from_str(app)?))
}

pub(super) fn verify_package_components(
    app: &AppConfig,
    packages: &std::collections::BTreeMap<
        wamn_package::PackageName,
        wamn_component_policy::PackageManifest,
    >,
) -> anyhow::Result<()> {
    let expected = crate::app_chart::plan::published_components(packages).count();
    ensure!(
        app.components.len() == expected,
        "rollback application has a different package component set"
    );
    for component in &app.components {
        let manifest = packages
            .get(&component.package)
            .context("rollback application component names an absent package")?;
        ensure!(
            manifest
                .deploy
                .as_ref()
                .is_some_and(|deploy| deploy.components.values().any(|declared| {
                    crate::app_chart::plan::component_name(
                        &component.package,
                        &declared.descriptor.component,
                    ) == component.name
                        && declared.descriptor.component_digest == component.digest
                })),
            "rollback application component {} differs from its package",
            component.name
        );
    }
    Ok(())
}

pub(super) async fn run(
    platform: &Platform,
    release: &str,
    revision: u32,
    deadline: Instant,
    readiness: Duration,
) -> anyhow::Result<()> {
    platform
        .cluster
        .rollback(release, revision, deadline, readiness)
        .await
}
