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
use plan::{Recovery, application_recovery_revision, recovery};

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
    /// Use the watcher for application operations bounded by the deploy Job deadline.
    WatcherUntil(Instant),
}

fn timeout_seconds(operation: &str, wait: Wait, now: Instant) -> anyhow::Result<u64> {
    const DEFAULT_TIMEOUT_SECONDS: u64 = 600;
    const MINIMUM_UPGRADE_BUDGET_SECONDS: u64 = 120;
    let seconds = match wait {
        Wait::WatcherUntil(deadline) => {
            let remaining = deadline.saturating_duration_since(now).as_secs();
            let budget = if operation == "upgrade" {
                ensure!(
                    remaining >= MINIMUM_UPGRADE_BUDGET_SECONDS,
                    "at least {MINIMUM_UPGRADE_BUDGET_SECONDS}s must remain for Helm upgrade and rollback ({remaining}s left)"
                );
                // Helm gives its rollback or failed-install cleanup this same timeout.
                remaining / 2
            } else {
                remaining
            };
            budget.min(DEFAULT_TIMEOUT_SECONDS)
        }
        Wait::None | Wait::Watcher => DEFAULT_TIMEOUT_SECONDS,
        Wait::Platform { timeout, .. } => timeout.as_secs(),
    };
    ensure!(seconds > 0, "no time left for Helm {operation}");
    Ok(seconds)
}

fn mutation_arguments<'a>(
    mut args: Vec<&'a OsStr>,
    operation: &str,
    wait: Wait,
    timeout: &'a str,
) -> Vec<&'a OsStr> {
    if !matches!(wait, Wait::None) {
        args.push(OsStr::new("--wait=watcher"));
    }
    if operation == "upgrade" {
        match wait {
            Wait::WatcherUntil(_) => args.push(OsStr::new("--rollback-on-failure")),
            Wait::Platform { jobs: true, .. } => args.push(OsStr::new("--wait-for-jobs")),
            _ => {}
        }
    }
    args.extend([OsStr::new("--timeout"), OsStr::new(timeout)]);
    args
}

async fn mutation_output(
    target: &Target,
    namespace: &str,
    name: &str,
    operation: &str,
    arguments: &[&OsStr],
    wait: Wait,
) -> anyhow::Result<Output> {
    let mut args = target_arguments(
        target,
        namespace,
        &[OsStr::new(operation), OsStr::new(name)],
    );
    args.extend_from_slice(arguments);
    let seconds = timeout_seconds(operation, wait, Instant::now())
        .with_context(|| format!("prepare Helm {operation} for {namespace}/{name}"))?;
    let timeout = format!("{seconds}s");
    let args = mutation_arguments(args, operation, wait, &timeout);
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

/// Install or upgrade one release, applying the values files in order.
/// A Job-bounded upgrade asks Helm to roll back on failure.
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
    // Helm reports the upgrade error and any rollback error together on stderr.
    stdout(output, "helm").map(drop)
}

/// Recover an interrupted release operation under its lifecycle lock.
///
/// A Job-bounded application deploy also recovers a failed revision left by
/// interrupted cleanup. Other callers recover pending operations only.
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
    let application = matches!(wait, Wait::WatcherUntil(_));
    let mut arguments = target_arguments(
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
    );
    if application {
        arguments.push("--failed".as_ref());
    }
    let listed = run_text(&arguments).await?;
    let releases: Vec<serde_json::Value> =
        serde_json::from_str(&listed).context("decode helm list")?;
    let Some(status) = releases
        .iter()
        .find(|release| release["name"] == name)
        .and_then(|release| release["status"].as_str())
    else {
        return Ok(());
    };
    match recovery(status, application) {
        Some(Recovery::Uninstall) => uninstall(target, namespace, name, wait).await,
        Some(Recovery::Rollback) if application => {
            let history = run_text(&target_arguments(
                target,
                namespace,
                &[
                    "history".as_ref(),
                    name.as_ref(),
                    "-o".as_ref(),
                    "json".as_ref(),
                ],
            ))
            .await?;
            let history =
                serde_json::from_str::<Vec<Value>>(&history).context("decode helm history")?;
            match application_recovery_revision(&history)
                .with_context(|| format!("recover Helm release {namespace}/{name}"))?
            {
                Some(revision) => rollback(target, namespace, name, Some(revision), wait).await,
                None => uninstall(target, namespace, name, wait).await,
            }
        }
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
    use serde_json::json;

    #[test]
    fn application_upgrade_requests_rollback_with_the_watcher() {
        let wait = Wait::WatcherUntil(Instant::now() + Duration::from_secs(120));
        let arguments = mutation_arguments(Vec::new(), "upgrade", wait, "60s");
        assert!(arguments.contains(&OsStr::new("--rollback-on-failure")));
        assert!(arguments.contains(&OsStr::new("--wait=watcher")));
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--timeout", "60s"])
        );

        for (operation, wait) in [
            ("rollback", wait),
            ("uninstall", wait),
            ("upgrade", Wait::None),
        ] {
            let arguments = mutation_arguments(Vec::new(), operation, wait, "60s");
            assert!(!arguments.contains(&OsStr::new("--rollback-on-failure")));
        }
    }

    #[test]
    fn application_upgrade_reserves_rollback_time_and_requires_a_minimum_budget() {
        let now = Instant::now();
        let timeout = |operation, remaining| {
            let wait = Wait::WatcherUntil(now + Duration::from_secs(remaining));
            timeout_seconds(operation, wait, now)
        };
        assert!(timeout("upgrade", 119).is_err());
        assert_eq!(timeout("upgrade", 120).unwrap(), 60);
        assert_eq!(timeout("upgrade", 123).unwrap(), 61);
        assert_eq!(timeout("upgrade", 1800).unwrap(), 600);
        assert_eq!(timeout("rollback", 119).unwrap(), 119);
        assert_eq!(timeout("uninstall", 119).unwrap(), 119);
    }

    #[test]
    fn pending_and_failed_operations_are_recovered() {
        assert_eq!(recovery("pending-install", true), Some(Recovery::Uninstall));
        assert_eq!(recovery("pending-upgrade", true), Some(Recovery::Rollback));
        assert_eq!(recovery("pending-rollback", true), Some(Recovery::Rollback));
        assert_eq!(recovery("deployed", true), None);
        assert_eq!(recovery("failed", false), None);
        assert_eq!(recovery("failed", true), Some(Recovery::Rollback));
    }

    #[test]
    fn application_recovery_restores_the_last_success_after_an_interrupted_rollback() {
        let history = [
            json!({"revision":1,"status":"superseded","description":"Install complete"}),
            json!({"revision":2,"status":"superseded","description":"Upgrade complete"}),
            json!({"revision":3,"status":"superseded","description":"Upgrade \"app\" failed: timeout"}),
            json!({"revision":4,"status":"failed","description":"Rollback \"app\" failed: update"}),
            json!({"revision":5,"status":"pending-rollback","description":"Rollback to 2"}),
        ];
        assert_eq!(application_recovery_revision(&history).unwrap(), Some(2));
    }

    #[test]
    fn application_recovery_uninstalls_a_failed_first_install() {
        let first = json!({"revision":1,"status":"failed"});
        assert_eq!(application_recovery_revision(&[first]).unwrap(), None);
        let later = json!({"revision":2,"status":"failed"});
        assert!(application_recovery_revision(&[later]).is_err());
    }
}
