//! The shared Helm command runner.
pub(super) mod plan;
use anyhow::{Context as _, ensure};
use serde_json::Value;
use std::{ffi::OsStr, process::Stdio};

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
}

impl Wait {
    fn argument(self) -> Option<&'static OsStr> {
        match self {
            Self::None => None,
            Self::Watcher => Some(OsStr::new("--wait=watcher")),
        }
    }
}

/// Install or upgrade one Helm owner and leave a failed revision available for inspection.
pub async fn upgrade(
    target: &crate::Target,
    chart: &std::path::Path,
    name: &str,
    values: &std::path::Path,
    timeout: std::time::Duration,
    wait: Wait,
) -> anyhow::Result<()> {
    upgrade_with_readiness(target, chart, name, values, timeout, wait, true).await
}

/// Submit a release whose workload needs the caller's post-upgrade SQL.
pub async fn upgrade_without_wait(
    target: &crate::Target,
    chart: &std::path::Path,
    name: &str,
    values: &std::path::Path,
    timeout: std::time::Duration,
) -> anyhow::Result<()> {
    upgrade_with_readiness(target, chart, name, values, timeout, Wait::None, false).await
}

/// Upgrade a release in its provisioned org namespace and wait with the watcher.
pub async fn upgrade_watched(
    target: &crate::Target,
    chart: &std::path::Path,
    name: &str,
    values: &std::path::Path,
    timeout: std::time::Duration,
) -> anyhow::Result<()> {
    upgrade_with_readiness(target, chart, name, values, timeout, Wait::Watcher, false).await
}

/// Change explicit values on an existing release without reading them back into the caller.
pub async fn update_values(
    target: &Target,
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
        target.namespace.as_ref(),
        name.as_ref(),
        chart.as_os_str(),
        "--reuse-values".as_ref(),
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

async fn upgrade_with_readiness(
    target: &crate::Target,
    chart: &std::path::Path,
    name: &str,
    values: &std::path::Path,
    timeout: std::time::Duration,
    wait: Wait,
    create_namespace: bool,
) -> anyhow::Result<()> {
    let timeout = format!("{}s", timeout.as_secs());
    let mut arguments = vec![
        "--kubeconfig".as_ref(),
        target.kubeconfig.as_os_str(),
        "--kube-context".as_ref(),
        target.context.as_ref(),
        "upgrade".as_ref(),
        "--install".as_ref(),
        "--namespace".as_ref(),
        target.namespace.as_ref(),
        name.as_ref(),
        chart.as_os_str(),
        "-f".as_ref(),
        values.as_os_str(),
        "--timeout".as_ref(),
        timeout.as_ref(),
        "--history-max".as_ref(),
        "50".as_ref(),
    ];
    arguments.extend(wait.argument());
    if create_namespace {
        arguments.push("--create-namespace".as_ref());
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
pub async fn recover_pending(target: &Target, name: &str, wait: Wait) -> anyhow::Result<()> {
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
        Some(Recovery::Uninstall) => uninstall(target, name, wait).await,
        Some(Recovery::Rollback) => {
            let mut arguments = vec![
                "rollback".as_ref(),
                name.as_ref(),
                "--kubeconfig".as_ref(),
                target.kubeconfig.as_os_str(),
                "--kube-context".as_ref(),
                target.context.as_ref(),
                "--namespace".as_ref(),
                target.namespace.as_ref(),
            ];
            arguments.extend(wait.argument());
            run(&arguments).await.map(drop)
        }
        None => Ok(()),
    }
}

/// Uninstall a release with its owner's wait strategy.
pub async fn uninstall(target: &Target, name: &str, wait: Wait) -> anyhow::Result<()> {
    let mut arguments = vec![
        "uninstall".as_ref(),
        name.as_ref(),
        "--kubeconfig".as_ref(),
        target.kubeconfig.as_os_str(),
        "--kube-context".as_ref(),
        target.context.as_ref(),
        "--namespace".as_ref(),
        target.namespace.as_ref(),
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
