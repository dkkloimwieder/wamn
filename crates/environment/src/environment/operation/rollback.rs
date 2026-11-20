//! Resolve an explicit Helm rollback target under the worker's lifecycle lock.

use std::{ffi::OsStr, num::NonZeroU32};

use anyhow::{Context as _, ensure};
use serde::Deserialize as _;
use serde_json::Value;
use wamn_digest::Digest;
use wamn_engine::application_config::{APPLICATION_CONFIG, ApplicationConfig};
use wamn_kubernetes::helm;
use wamn_registry::ApplicationName;
use wamn_registry::Triple;

use super::Platform;

async fn helm(platform: &Platform, arguments: &[&str]) -> anyhow::Result<std::process::Output> {
    let target = &platform.target;
    let namespace = helm::org_release_namespace(&target.namespace);
    let mut args = vec![
        OsStr::new("--kubeconfig"),
        target.kubeconfig.as_os_str(),
        OsStr::new("--kube-context"),
        target.context.as_ref(),
        OsStr::new("--namespace"),
        OsStr::new(namespace.as_ref()),
    ];
    args.extend(arguments.iter().map(OsStr::new));
    helm::output(&args).await
}

async fn get(platform: &Platform, args: &[&str]) -> anyhow::Result<Option<Vec<u8>>> {
    let output = helm(platform, args).await?;
    if output.status.success() {
        return Ok(Some(output.stdout));
    }
    let error = String::from_utf8_lossy(&output.stderr);
    if error.contains("release: not found") || error.contains("release not found") {
        return Ok(None);
    }
    anyhow::bail!("helm {} failed: {}", args.join(" "), error.trim())
}

pub(super) async fn target(
    platform: &Platform,
    triple: &Triple,
    application: &ApplicationName,
    release: &str,
    revision: Option<NonZeroU32>,
) -> anyhow::Result<u32> {
    if let Some(revision) = revision {
        return Ok(revision.get());
    }
    let api = wamn_kubernetes::kubernetes_api::KubernetesApi::in_cluster()?;
    let live = crate::environment::document::workload_release(&api, triple, application)
        .await?
        .context("application has no deployed release")?
        .revision;
    let name = format!("{release}-http");
    for candidate in (1..live).rev() {
        let Some(manifest) = get(
            platform,
            &[
                "get",
                "manifest",
                release,
                "--revision",
                &candidate.to_string(),
            ],
        )
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
                    return Ok(candidate);
                }
                break;
            }
        }
    }
    anyhow::bail!("live content revision {live} has nothing older to roll back to")
}

/// Read the target's package pin and the application configuration Helm will restore.
pub(super) async fn application(
    platform: &Platform,
    release: &str,
    revision: u32,
) -> anyhow::Result<(Digest, ApplicationConfig)> {
    let values = get(
        platform,
        &[
            "get",
            "values",
            release,
            "--revision",
            &revision.to_string(),
            "-o",
            "json",
        ],
    )
    .await?
    .with_context(|| format!("Helm revision {revision} is absent"))?;
    let values: Value = serde_json::from_slice(&values)?;
    let digest = values["release"]["manifestDigest"]
        .as_str()
        .context("rollback revision has no package digest")?
        .parse()?;
    let app = values["roles"][0]["config"][APPLICATION_CONFIG]
        .as_str()
        .context("rollback revision has no wamn.application configuration")?;
    Ok((digest, serde_json::from_str(app)?))
}

pub(super) fn verify_package_components(
    application: &ApplicationConfig,
    packages: &std::collections::BTreeMap<
        wamn_package::PackageName,
        wamn_component_policy::PackageManifest,
    >,
) -> anyhow::Result<()> {
    let expected = crate::release_chart::published_components(packages).count();
    ensure!(
        application.components.len() == expected,
        "rollback application has a different package component set"
    );
    for component in &application.components {
        let manifest = packages
            .get(&component.package)
            .context("rollback application component names an absent package")?;
        ensure!(
            manifest
                .deploy
                .as_ref()
                .is_some_and(|deploy| deploy.components.values().any(|declared| {
                    crate::release_chart::component_name(
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

pub(super) async fn run(platform: &Platform, release: &str, revision: u32) -> anyhow::Result<()> {
    let output = helm(
        platform,
        &[
            "rollback",
            release,
            &revision.to_string(),
            "--server-side=true",
            "--wait=watcher",
            "--timeout",
            "600s",
        ],
    )
    .await?;
    ensure!(
        output.status.success(),
        "helm rollback failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
