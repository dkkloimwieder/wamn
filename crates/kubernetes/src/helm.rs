//! The shared Helm command runner.
pub(super) mod plan;
use anyhow::{Context as _, ensure};
use serde_json::Value;
use std::{borrow::Cow, ffi::OsStr, process::Stdio, time::Duration};
use tokio::time::Instant;

/// The Helm storage namespace for releases owned by an org.
pub fn org_release_namespace(org_namespace: &str) -> Cow<'_, str> {
    Cow::Owned(format!("wamn-helm-{org_namespace}"))
}

pub async fn run(arguments: &[&OsStr]) -> anyhow::Result<Vec<u8>> {
    let output = output(arguments).await?;
    ensure!(
        output.status.success(),
        "helm failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

/// Run Helm and retain its exit status for an inspection caller.
pub async fn output(arguments: &[&OsStr]) -> anyhow::Result<std::process::Output> {
    tokio::process::Command::new("helm")
        .args(arguments)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("start helm")
}

/// Pull and unpack a chart at the selected registry reference.
pub fn pull_chart(
    reference: &str,
    directory: &std::path::Path,
    plain_http: bool,
) -> anyhow::Result<()> {
    let mut command = std::process::Command::new("helm");
    command
        .args(["pull", reference, "--untar", "--untardir"])
        .arg(directory);
    if plain_http {
        command.arg("--plain-http");
    }
    if let Some(configuration) = std::env::var_os("WAMN_REGISTRY_AUTH_FILE") {
        command.arg("--registry-config").arg(configuration);
    }
    let output = command.output().context("fetch the selected chart")?;
    ensure!(
        output.status.success(),
        "fetch chart: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

pub fn template(entry: &serde_json::Value) -> anyhow::Result<Vec<serde_json::Value>> {
    use serde::Deserialize as _;
    use std::io::Write as _;
    use std::process::Command;
    let chart = entry
        .get("render_chart")
        .unwrap_or(&entry["chart"])
        .as_str()
        .context("local chart reference")?;
    anyhow::ensure!(
        std::path::Path::new(chart).exists(),
        "local chart {chart} is missing; rendering cannot fetch a chart"
    );
    let shown = Command::new("helm")
        .args(["show", "chart", chart])
        .output()
        .context("read local Helm chart")?;
    anyhow::ensure!(shown.status.success(), "read local chart {chart}");
    let metadata: Value = serde_yaml::from_slice(&shown.stdout)?;
    anyhow::ensure!(
        metadata["version"] == entry["version"],
        "chart {chart} version {} does not match pinned {}",
        metadata["version"],
        entry["version"]
    );
    let name = entry["name"].as_str().context("chart release name")?;
    let namespace = entry["namespace"].as_str().context("chart namespace")?;
    let mut child = Command::new("helm")
        .args(["template", name, chart, "--namespace", namespace, "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start local helm template")?;
    child
        .stdin
        .take()
        .context("helm stdin")?
        .write_all(&serde_json::to_vec(&entry["values"])?)?;
    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "helm template {chart}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_yaml::Deserializer::from_slice(&output.stdout)
        .map(serde_json::Value::deserialize)
        .filter_map(|value| match value {
            Ok(value) if value.is_null() => None,
            value => Some(value.map_err(Into::into)),
        })
        .collect()
}

/// Readiness strategy for a Helm release.
#[derive(Clone, Copy, Debug)]
pub enum Wait {
    None,
    Watcher,
    Platform {
        timeout: Duration,
        jobs: bool,
    },
    /// Use the watcher and cap the upgrade timeout at this deadline.
    WatcherUntil(Instant),
}

impl Wait {
    fn argument(self) -> Option<&'static OsStr> {
        match self {
            Self::None => None,
            Self::Watcher | Self::Platform { .. } | Self::WatcherUntil(_) => {
                Some(OsStr::new("--wait=watcher"))
            }
        }
    }
}

/// Prerequisites for the install path of an upgrade.
#[derive(Clone, Copy, Debug)]
pub enum Install {
    CreateNamespace,
    ExistingNamespace,
    SkipCrds,
}

/// Change explicit values on an existing release without reading them back into the caller.
pub async fn update_values(
    target: &Target,
    namespace: &str,
    chart: &std::path::Path,
    name: &str,
    values: &std::path::Path,
) -> anyhow::Result<()> {
    run(&[
        "--kubeconfig".as_ref(),
        target.kubeconfig.as_os_str(),
        "--kube-context".as_ref(),
        target.context.as_ref(),
        "upgrade".as_ref(),
        "--namespace".as_ref(),
        namespace.as_ref(),
        name.as_ref(),
        chart.as_os_str(),
        "--reuse-values".as_ref(),
        "--server-side=true".as_ref(),
        "-f".as_ref(),
        values.as_os_str(),
        "--timeout".as_ref(),
        "600s".as_ref(),
        "--history-max".as_ref(),
        "50".as_ref(),
    ])
    .await
    .map(drop)
}

/// Install or upgrade one Helm release and leave failed revisions available for inspection.
/// Helm applies the values files in the given order.
pub async fn upgrade(
    target: &Target,
    namespace: &str,
    chart: &std::path::Path,
    name: &str,
    values: &[&std::path::Path],
    wait: Wait,
    install: Install,
) -> anyhow::Result<()> {
    let timeout = match wait {
        Wait::WatcherUntil(deadline) => deadline
            .saturating_duration_since(Instant::now())
            .as_secs()
            .min(600),
        Wait::None | Wait::Watcher => 600,
        Wait::Platform { timeout, .. } => timeout.as_secs(),
    };
    ensure!(
        timeout > 0,
        "no time left to upgrade Helm release {namespace}/{name}"
    );
    let timeout = format!("{timeout}s");
    let mut arguments = vec![
        "--kubeconfig".as_ref(),
        target.kubeconfig.as_os_str(),
        "--kube-context".as_ref(),
        target.context.as_ref(),
        "upgrade".as_ref(),
        "--install".as_ref(),
        "--server-side=true".as_ref(),
        "--namespace".as_ref(),
        namespace.as_ref(),
        name.as_ref(),
        chart.as_os_str(),
        "--timeout".as_ref(),
        timeout.as_ref(),
        "--history-max".as_ref(),
        "50".as_ref(),
    ];
    for file in values {
        arguments.extend([OsStr::new("-f"), file.as_os_str()]);
    }
    arguments.extend(wait.argument());
    if matches!(wait, Wait::Platform { jobs: true, .. }) {
        arguments.push("--wait-for-jobs".as_ref());
    }
    match install {
        Install::CreateNamespace => arguments.push("--create-namespace".as_ref()),
        Install::SkipCrds => arguments.push("--skip-crds".as_ref()),
        Install::ExistingNamespace => {}
    }
    run(&arguments).await.map(drop)
}

use crate::Target;
use plan::{Recovery, recovery};

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
pub async fn recover_pending(
    target: &Target,
    namespace: &str,
    name: &str,
    wait: Wait,
) -> anyhow::Result<()> {
    let filter = format!("^{name}$");
    let listed = run_text(&[
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
        namespace.as_ref(),
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
        Some(Recovery::Uninstall) => uninstall(target, namespace, name, wait).await,
        Some(Recovery::Rollback) => {
            let mut arguments = vec![
                "rollback".as_ref(),
                name.as_ref(),
                "--server-side=true".as_ref(),
                "--kubeconfig".as_ref(),
                target.kubeconfig.as_os_str(),
                "--kube-context".as_ref(),
                target.context.as_ref(),
                "--namespace".as_ref(),
                namespace.as_ref(),
            ];
            arguments.extend(wait.argument());
            run(&arguments).await.map(drop)
        }
        None => Ok(()),
    }
}

/// Uninstall a release with its owner's wait strategy.
pub async fn uninstall(
    target: &Target,
    namespace: &str,
    name: &str,
    wait: Wait,
) -> anyhow::Result<()> {
    let mut arguments = vec![
        "uninstall".as_ref(),
        name.as_ref(),
        "--kubeconfig".as_ref(),
        target.kubeconfig.as_os_str(),
        "--kube-context".as_ref(),
        target.context.as_ref(),
        "--namespace".as_ref(),
        namespace.as_ref(),
        "--ignore-not-found".as_ref(),
    ];
    arguments.extend(wait.argument());
    run(&arguments).await.map(drop)
}

pub async fn run_text(arguments: &[&std::ffi::OsStr]) -> anyhow::Result<String> {
    String::from_utf8(run(arguments).await?).context("helm wrote text")
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
}
