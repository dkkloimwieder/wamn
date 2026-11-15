//! The `release` chart of one application
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! An application is one Helm release of `deploy/platform/release`: the pinned
//! workload and route resources. The environment chart owns the host.
//! This module derives the release name, renders the install values, stamps the
//! platform image set into the chart's defaults,
//! reads them back, and runs the Helm install and uninstall.
//!
//! The platform image set lives only in the chart's default values (R15). The
//! install values carry no image digest and no integer release id: the release
//! is named by its manifest digest alone (R1).

pub(super) mod plan;
use plan::recovery;
pub use plan::{release_name, roles, values, workload_name};

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use wamn_catalog::{AttachmentType, ServingManifest};

/// The platform version: `version` in the `Chart.yaml` of the release chart
/// directory `chart`. The chart is its one owner, so a binary does not change
/// with the version (wamn-6qo7v).
pub fn chart_version(chart: &Path) -> anyhow::Result<String> {
    read_platform_version(&chart.join("Chart.yaml"))
}

/// Read the platform version from the release chart's top-level version.
///
/// # Errors
///
/// Returns an error naming the path if the chart cannot be read or decoded.
pub fn read_platform_version(path: &Path) -> anyhow::Result<String> {
    #[derive(Deserialize)]
    struct Chart {
        version: String,
    }

    let source = std::fs::read_to_string(path)
        .with_context(|| format!("read platform chart {}", path.display()))?;
    let chart: Chart = serde_yaml::from_str(&source).with_context(|| {
        format!(
            "read top-level version from platform chart {}",
            path.display()
        )
    })?;
    anyhow::ensure!(
        !chart.version.trim().is_empty(),
        "platform chart {} has an empty version",
        path.display()
    );
    Ok(chart.version)
}

pub use plan::ImageSet;

/// Copy `chart` to `out` with the image set written into its defaults.
/// Host runtime defaults belong to the environment chart.
pub fn stamp(chart: &Path, set: &ImageSet, out: &Path) -> anyhow::Result<PathBuf> {
    let host: oci_client::Reference = set.host.parse().context("parse the host image")?;
    wamn_package_artifact::ensure_digest(
        host.digest().context("the host image requires a digest")?,
    )?;
    ensure!(host.tag().is_some(), "the host image requires a tag");
    let pinned = [("http", &set.http)];
    for (name, image) in pinned {
        ensure!(
            image.contains("@sha256:"),
            "the {name} image {image} is not pinned by digest"
        );
    }
    copy_dir(chart, out)?;
    let path = out.join("values.yaml");
    let mut values: Value = serde_yaml::from_str(
        &std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?,
    )
    .context("parse the chart's default values")?;
    let platform = &mut values["platform"];
    platform["host"]["image"] = set.host.clone().into();
    platform["roles"]["http"]["image"] = set.http.clone().into();
    std::fs::write(&path, serde_yaml::to_string(&values)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(out.to_owned())
}

/// The default values of `chart`, with `helm show values`.
async fn default_values(chart: &Path) -> anyhow::Result<Value> {
    let shown = helm(&["show".as_ref(), "values".as_ref(), chart.as_os_str()]).await?;
    serde_yaml::from_str(&shown).context("parse the chart's values")
}

/// A stamped default of the chart, which must not be empty.
fn stamped(value: &Value, name: &str) -> anyhow::Result<String> {
    let text = value.as_str().unwrap_or_default();
    ensure!(
        !text.is_empty(),
        "the chart's default {name} is not stamped"
    );
    Ok(text.to_owned())
}

/// Read the image set from the default values of `chart`.
pub async fn image_set(chart: &Path) -> anyhow::Result<ImageSet> {
    let values = default_values(chart).await?;
    let host = stamped(&values["platform"]["host"]["image"], "host image")?;
    let roles = &values["platform"]["roles"];
    Ok(ImageSet {
        host,
        http: stamped(&roles["http"]["image"], "http image")?,
    })
}

pub use plan::Role;

pub use plan::RoleName;

pub use plan::TELEMETRY_VARIABLES;

pub use plan::ValuesInput;

/// Where Helm writes: an explicit kubeconfig, context and namespace.
pub use wamn_kubernetes::Target;

/// Install or upgrade one application's release at the version of `chart` and
/// wait for its resources. `description` is kept with the revision.
pub async fn upgrade(
    target: &Target,
    chart: &Path,
    name: &str,
    values: &Path,
    timeout: Duration,
    description: &str,
) -> anyhow::Result<()> {
    let timeout = format!("{}s", timeout.as_secs());
    let version = chart_version(chart)?;
    helm(&[
        "upgrade".as_ref(),
        "--install".as_ref(),
        name.as_ref(),
        chart.as_os_str(),
        "--version".as_ref(),
        version.as_ref(),
        "--description".as_ref(),
        description.as_ref(),
        "--kubeconfig".as_ref(),
        target.kubeconfig.as_os_str(),
        "--kube-context".as_ref(),
        target.context.as_ref(),
        "--namespace".as_ref(),
        target.namespace.as_ref(),
        "-f".as_ref(),
        values.as_os_str(),
        "--skip-crds".as_ref(),
        "--wait".as_ref(),
        "--timeout".as_ref(),
        timeout.as_ref(),
        "--history-max".as_ref(),
        "50".as_ref(),
    ])
    .await
    .map(drop)
}

use plan::Recovery;

/// Clear a pending operation on one application's release (wamn-jovpg).
///
/// Only the apply of this application writes its release, and it holds the
/// install lock, so a pending operation here belongs to a writer that died,
/// for example an install Job deleted while Helm waited. Helm refuses every
/// later upgrade with "another operation (install/upgrade/rollback) is in
/// progress" until the pending revision is cleared.
///
/// # Errors
///
/// When Helm fails.
pub async fn recover_pending(target: &Target, name: &str) -> anyhow::Result<()> {
    let filter = format!("^{name}$");
    let listed = helm(&[
        "list".as_ref(),
        "--pending".as_ref(),
        "--filter".as_ref(),
        filter.as_ref(),
        "-o".as_ref(),
        "json".as_ref(),
        "--kubeconfig".as_ref(),
        target.kubeconfig.as_os_str(),
        "--kube-context".as_ref(),
        target.context.as_ref(),
        "--namespace".as_ref(),
        target.namespace.as_ref(),
    ])
    .await?;
    let releases: Vec<serde_json::Value> =
        serde_json::from_str(&listed).context("decode helm list")?;
    let Some(status) = releases
        .iter()
        .find(|release| release["name"] == name)
        .and_then(|release| release["status"].as_str())
    else {
        return Ok(());
    };
    match recovery(status) {
        Some(Recovery::Uninstall) => uninstall(target, name).await,
        Some(Recovery::Rollback) => helm(&[
            "rollback".as_ref(),
            name.as_ref(),
            "--kubeconfig".as_ref(),
            target.kubeconfig.as_os_str(),
            "--kube-context".as_ref(),
            target.context.as_ref(),
            "--namespace".as_ref(),
            target.namespace.as_ref(),
            "--wait".as_ref(),
        ])
        .await
        .map(drop),
        None => Ok(()),
    }
}

/// Uninstall one application's release and wait for its objects to go.
pub async fn uninstall(target: &Target, name: &str) -> anyhow::Result<()> {
    helm(&[
        "uninstall".as_ref(),
        name.as_ref(),
        "--kubeconfig".as_ref(),
        target.kubeconfig.as_os_str(),
        "--kube-context".as_ref(),
        target.context.as_ref(),
        "--namespace".as_ref(),
        target.namespace.as_ref(),
        "--wait".as_ref(),
    ])
    .await
    .map(drop)
}

async fn helm(arguments: &[&std::ffi::OsStr]) -> anyhow::Result<String> {
    String::from_utf8(wamn_kubernetes::helm::run(arguments).await?).context("helm wrote text")
}

fn copy_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("create {}", to.display()))?;
    for entry in std::fs::read_dir(from).with_context(|| format!("read {}", from.display()))? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("copy {}", entry.path().display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn only_a_pending_operation_is_recovered() {
        assert_eq!(recovery("pending-install"), Some(Recovery::Uninstall));
        assert_eq!(recovery("pending-upgrade"), Some(Recovery::Rollback));
        assert_eq!(recovery("pending-rollback"), Some(Recovery::Rollback));
        assert_eq!(recovery("deployed"), None);
        assert_eq!(recovery("failed"), None);
    }

    const DIGEST: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn input() -> ValuesInput {
        ValuesInput {
            org: "acme".into(),
            project: "wms".into(),
            env: "prod".into(),
            manifest_digest: DIGEST.into(),
            artifact_base: "registry.example/releases".into(),
            route_host: "wms.acme.example".into(),
            applications: [(
                "wms".to_owned(),
                wamn_catalog::ApplicationDefinition {
                    connections: std::collections::BTreeMap::new(),
                    packages: ["wamn_wms".to_owned()].into(),
                },
            )]
            .into(),
            roles: vec![Role {
                entry: Some(format!("entries@sha256:{}", "a".repeat(64))),
                name: RoleName::Http,
                config: Mapping::new(),
            }],
            actor: "ops".into(),
            component_base: "registry.example/components".into(),
        }
    }

    #[test]
    fn values_require_one_application() {
        let mut input = input();
        input.manifest_digest = "not-a-digest".into();
        assert!(values(&input).is_err());
        input.manifest_digest = DIGEST.into();
        let (name, application) = input.applications.pop_first().unwrap();
        assert!(values(&input).is_err());
        input.applications.insert(name, application.clone());
        input.applications.insert("another".into(), application);
        assert!(values(&input).is_err());
    }

    #[test]
    fn values_refuse_an_entry_without_a_component_base() {
        let mut input = input();
        input.component_base.clear();
        assert!(values(&input).is_err());
    }
}
