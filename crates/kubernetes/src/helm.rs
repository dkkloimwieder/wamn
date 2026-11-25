//! The shared Helm command runner.
pub(super) mod plan;
use anyhow::{Context as _, ensure};
use serde_json::Value;
use std::{
    borrow::Cow,
    ffi::OsStr,
    process::{Output, Stdio},
    time::Duration,
};
use tokio::time::Instant;

use crate::Target;
use plan::{Recovery, recovery};

/// The Helm storage namespace for releases owned by an org.
pub fn org_release_namespace(org_namespace: &str) -> Cow<'_, str> {
    Cow::Owned(format!("wamn-helm-{org_namespace}"))
}

pub async fn run(arguments: &[&OsStr]) -> anyhow::Result<Vec<u8>> {
    stdout(output(arguments).await?, "helm")
}

fn stdout(output: Output, operation: &str) -> anyhow::Result<Vec<u8>> {
    ensure!(
        output.status.success(),
        "{operation} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

fn target_arguments<'a>(
    target: &'a Target,
    namespace: &'a str,
    arguments: &[&'a OsStr],
) -> Vec<&'a OsStr> {
    let mut args = vec![
        OsStr::new("--kubeconfig"),
        target.kubeconfig.as_os_str(),
        OsStr::new("--kube-context"),
        OsStr::new(&target.context),
        OsStr::new("--namespace"),
        OsStr::new(namespace),
    ];
    args.extend_from_slice(arguments);
    args
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
    /// Use the watcher and cap the Helm timeout at this deadline.
    WatcherUntil(Instant),
}

async fn mutation_output(
    target: &Target,
    namespace: &str,
    name: &str,
    operation: &str,
    arguments: &[&OsStr],
    wait: Wait,
) -> anyhow::Result<Output> {
    const DEFAULT_TIMEOUT_SECONDS: u64 = 600;
    let seconds = match wait {
        Wait::WatcherUntil(deadline) => deadline
            .saturating_duration_since(Instant::now())
            .as_secs()
            .min(DEFAULT_TIMEOUT_SECONDS),
        Wait::None | Wait::Watcher => DEFAULT_TIMEOUT_SECONDS,
        Wait::Platform { timeout, .. } => timeout.as_secs(),
    };
    ensure!(
        seconds > 0,
        "no time left to {operation} Helm release {namespace}/{name}"
    );
    let timeout = format!("{seconds}s");
    let mut args = target_arguments(
        target,
        namespace,
        &[OsStr::new(operation), OsStr::new(name)],
    );
    args.extend_from_slice(arguments);
    if !matches!(wait, Wait::None) {
        args.push(OsStr::new("--wait=watcher"));
    }
    if operation == "upgrade" && matches!(wait, Wait::Platform { jobs: true, .. }) {
        args.push(OsStr::new("--wait-for-jobs"));
    }
    args.extend([OsStr::new("--timeout"), OsStr::new(&timeout)]);
    output(&args).await
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
    let output = mutation_output(
        target,
        namespace,
        name,
        "upgrade",
        &[
            chart.as_os_str(),
            "--reuse-values".as_ref(),
            "--server-side=true".as_ref(),
            "-f".as_ref(),
            values.as_os_str(),
            "--history-max".as_ref(),
            "50".as_ref(),
        ],
        Wait::None,
    )
    .await?;
    stdout(output, "helm").map(drop)
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
    let mut arguments = vec![
        "--install".as_ref(),
        "--server-side=true".as_ref(),
        chart.as_os_str(),
        "--history-max".as_ref(),
        "50".as_ref(),
    ];
    for file in values {
        arguments.extend([OsStr::new("-f"), file.as_os_str()]);
    }
    match install {
        Install::CreateNamespace => arguments.push("--create-namespace".as_ref()),
        Install::SkipCrds => arguments.push("--skip-crds".as_ref()),
        Install::ExistingNamespace => {}
    }
    let output = mutation_output(target, namespace, name, "upgrade", &arguments, wait).await?;
    stdout(output, "helm").map(drop)
}

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
    let listed = run_text(&target_arguments(
        target,
        namespace,
        &[
            "list".as_ref(),
            "--pending".as_ref(),
            "--filter".as_ref(),
            filter.as_ref(),
            "-o".as_ref(),
            "json".as_ref(),
        ],
    ))
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
        Some(Recovery::Rollback) => rollback(target, namespace, name, None, wait).await,
        None => Ok(()),
    }
}

pub(crate) async fn rollback(
    target: &Target,
    namespace: &str,
    name: &str,
    revision: Option<u32>,
    wait: Wait,
) -> anyhow::Result<()> {
    let operation = if revision.is_some() {
        "helm rollback"
    } else {
        "helm"
    };
    let revision = revision.map(|revision| revision.to_string());
    let mut arguments = Vec::new();
    arguments.extend(revision.as_deref().map(OsStr::new));
    arguments.push(OsStr::new("--server-side=true"));
    let output = mutation_output(target, namespace, name, "rollback", &arguments, wait).await?;
    stdout(output, operation).map(drop)
}

/// Uninstall a release with its owner's wait strategy.
pub async fn uninstall(
    target: &Target,
    namespace: &str,
    name: &str,
    wait: Wait,
) -> anyhow::Result<()> {
    let output = mutation_output(
        target,
        namespace,
        name,
        "uninstall",
        &[OsStr::new("--ignore-not-found")],
        wait,
    )
    .await?;
    stdout(output, "helm").map(drop)
}

pub async fn run_text(arguments: &[&std::ffi::OsStr]) -> anyhow::Result<String> {
    String::from_utf8(run(arguments).await?).context("helm wrote text")
}

pub(crate) async fn get(
    target: &Target,
    kind: &str,
    release: &str,
    revision: u32,
) -> anyhow::Result<Option<Vec<u8>>> {
    let namespace = org_release_namespace(&target.namespace);
    let revision = revision.to_string();
    let mut arguments = vec!["get", kind, release, "--revision", &revision];
    if kind == "values" {
        arguments.extend(["-o", "json"]);
    }
    let mut args = target_arguments(target, &namespace, &[]);
    args.extend(arguments.iter().map(OsStr::new));
    let output = output(&args).await?;
    if output.status.success() {
        return Ok(Some(output.stdout));
    }
    let error = String::from_utf8_lossy(&output.stderr);
    if error.contains("release: not found") || error.contains("release not found") {
        return Ok(None);
    }
    anyhow::bail!("helm {} failed: {}", arguments.join(" "), error.trim())
}

pub(crate) async fn project_release_exists(target: &Target, release: &str) -> anyhow::Result<bool> {
    let namespace = org_release_namespace(&target.namespace);
    let filter = format!("^{release}$");
    let output = run_text(&target_arguments(
        target,
        &namespace,
        &[
            "list".as_ref(),
            "--filter".as_ref(),
            filter.as_ref(),
            "-o".as_ref(),
            "json".as_ref(),
        ],
    ))
    .await
    .with_context(|| format!("look up project release {release}"))?;
    let releases: Vec<Value> = serde_json::from_str(&output).context("decode Helm release list")?;
    Ok(releases.iter().any(|entry| entry["name"] == release))
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
