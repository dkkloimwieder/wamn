//! The shared Helm command runner.
pub(super) mod plan;
use anyhow::{Context as _, ensure};
use serde_json::Value;
use std::{
    borrow::Cow,
    error::Error,
    ffi::OsStr,
    fmt,
    process::{Output, Stdio},
    time::Duration,
};
use tokio::time::{Instant, timeout_at};

use crate::Target;
use plan::{Recovery, app_recovery_revision, recovery};

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
    WatcherUntil {
        deadline: Instant,
        readiness: Duration,
    },
}

fn timeout_seconds(operation: &str, wait: Wait, now: Instant) -> anyhow::Result<u64> {
    const DEFAULT_TIMEOUT_SECONDS: u64 = 600;
    const MINIMUM_UPGRADE_BUDGET_SECONDS: u64 = 120;
    let seconds = match wait {
        Wait::WatcherUntil { deadline, .. } => {
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
            Wait::WatcherUntil { .. } => args.push(OsStr::new("--rollback-on-failure")),
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
) -> anyhow::Result<(Output, Instant)> {
    let mut args = target_arguments(
        target,
        namespace,
        &[OsStr::new(operation), OsStr::new(name)],
    );
    args.extend_from_slice(arguments);
    let start = Instant::now();
    let seconds = timeout_seconds(operation, wait, start)
        .with_context(|| format!("prepare Helm {operation} for {namespace}/{name}"))?;
    let timeout = format!("{seconds}s");
    let args = mutation_arguments(args, operation, wait, &timeout);
    Ok((output(&args).await?, start + Duration::from_secs(seconds)))
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
    let (output, _) = mutation_output(
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

/// An application upgrade failed and its previous release was restored.
#[derive(Debug)]
pub struct UpgradeRolledBack {
    release: String,
    revision: u32,
    source: anyhow::Error,
}

impl UpgradeRolledBack {
    pub fn new(namespace: &str, release: &str, revision: u32, source: anyhow::Error) -> Self {
        Self {
            release: format!("{namespace}/{release}"),
            revision,
            source,
        }
    }
}

impl fmt::Display for UpgradeRolledBack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Helm upgrade for {} failed and rolled back to revision {}",
            self.release, self.revision
        )
    }
}

impl Error for UpgradeRolledBack {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
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
    let previous = if matches!(wait, Wait::WatcherUntil { .. }) {
        arguments.extend([OsStr::new("--output"), OsStr::new("json")]);
        release_row(target, namespace, name, &["--deployed"])
            .await?
            .as_ref()
            .map(release_revision)
            .transpose()?
    } else {
        None
    };
    let (output, phase_end) =
        mutation_output(target, namespace, name, "upgrade", &arguments, wait).await?;
    let output = match stdout(output, "helm") {
        Ok(output) => output,
        Err(source) => {
            if let (Some(revision), Wait::WatcherUntil { deadline, .. }) = (previous, wait)
                && let Ok(Ok(history)) = timeout_at(
                    deadline - Duration::from_secs(1),
                    release_history(target, namespace, name),
                )
                .await
                && helm_rolled_back(&history, revision)
            {
                return Err(UpgradeRolledBack::new(namespace, name, revision, source).into());
            }
            // Helm reports the upgrade error and any rollback error together on stderr.
            return Err(source);
        }
    };
    let Wait::WatcherUntil {
        deadline,
        readiness,
    } = wait
    else {
        return Ok(());
    };
    let rollout = async {
        let candidate =
            serde_json::from_slice::<Value>(&output).context("decode the upgraded Helm release")?;
        let manifest = candidate["manifest"]
            .as_str()
            .context("upgraded Helm release has no manifest")?;
        wait_app(
            target,
            name,
            manifest.as_bytes(),
            phase_end.min(Instant::now() + readiness),
        )
        .await
    }
    .await;
    let Err(upgrade) = rollout else {
        return Ok(());
    };
    // Leave a second before the Job deadline to report both failures.
    let deadline = deadline - Duration::from_secs(1);
    let wait = Wait::WatcherUntil {
        deadline,
        readiness,
    };
    let restore = timeout_at(deadline, async {
        match previous {
            Some(revision) => rollback(target, namespace, name, Some(revision), wait).await,
            None => uninstall(target, namespace, name, wait).await,
        }
    })
    .await
    .context("Job budget expired restoring the Helm release")
    .flatten();
    let action = if previous.is_some() {
        "rollback"
    } else {
        "failed install cleanup"
    };
    match (restore, previous) {
        (Ok(()), Some(revision)) => {
            Err(UpgradeRolledBack::new(namespace, name, revision, upgrade).into())
        }
        (Ok(()), None) => Err(upgrade.context(format!(
            "Helm upgrade did not become Ready and {action} completed"
        ))),
        (Err(restore), _) => anyhow::bail!(
            "Helm upgrade did not become Ready: {upgrade:#}\nHelm {action} failed: {restore:#}"
        ),
    }
}

fn helm_rolled_back(history: &[Value], previous: u32) -> bool {
    let Some(latest) = history.iter().max_by_key(|row| row["revision"].as_u64()) else {
        return false;
    };
    let Some(failed) = latest["revision"].as_u64().and_then(|n| n.checked_sub(1)) else {
        return false;
    };
    // A successful Helm rollback appends a deployed revision with its target
    // in the description. The failed upgrade is the preceding revision.
    failed > u64::from(previous)
        && latest["status"] == "deployed"
        && latest["description"] == format!("Rollback to {previous}")
        && history
            .iter()
            .any(|row| row["revision"].as_u64() == Some(failed) && row["status"] == "failed")
}

async fn wait_app(
    target: &Target,
    release: &str,
    manifest: &[u8],
    deadline: Instant,
) -> anyhow::Result<()> {
    timeout_at(
        deadline,
        crate::deployment_components::wait_app_ready(target, manifest),
    )
    .await
    .with_context(|| {
        format!("release {release} did not serve its selected template before the rollout deadline")
    })?
}

async fn release_row(
    target: &Target,
    namespace: &str,
    name: &str,
    states: &[&str],
) -> anyhow::Result<Option<Value>> {
    let filter = format!("^{name}$");
    let mut arguments = target_arguments(
        target,
        namespace,
        &[
            "list".as_ref(),
            "--filter".as_ref(),
            filter.as_ref(),
            "-o".as_ref(),
            "json".as_ref(),
        ],
    );
    arguments.extend(states.iter().map(OsStr::new));
    let listed = run_text(&arguments).await?;
    let releases: Vec<Value> = serde_json::from_str(&listed).context("decode helm list")?;
    Ok(releases.into_iter().find(|release| release["name"] == name))
}

fn release_revision(row: &Value) -> anyhow::Result<u32> {
    row["revision"]
        .as_str()
        .context("Helm release revision is absent")?
        .parse()
        .context("invalid Helm release revision")
}

async fn release_history(
    target: &Target,
    namespace: &str,
    name: &str,
) -> anyhow::Result<Vec<Value>> {
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
    serde_json::from_str(&history).context("decode Helm history")
}

/// Recover an interrupted release operation under its lifecycle lock.
///
/// Also recover failed application revisions and deployed application revisions
/// whose templates are missing or unready.
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
    let app = matches!(wait, Wait::WatcherUntil { .. });
    let filters: &[_] = if app {
        &["--pending", "--failed", "--deployed"]
    } else {
        &["--pending"]
    };
    let Some(entry) = release_row(target, namespace, name, filters).await? else {
        return Ok(());
    };
    let status = entry["status"].as_str().unwrap_or_default();
    match recovery(status, app) {
        Some(Recovery::Uninstall) => return uninstall(target, namespace, name, wait).await,
        Some(Recovery::Rollback) if !app => {
            return rollback(target, namespace, name, None, wait).await;
        }
        None if !app || status != "deployed" => return Ok(()),
        _ => {}
    }

    let interrupted = release_revision(&entry)?;
    if status == "deployed" {
        let manifest = get(target, "manifest", name, interrupted)
            .await?
            .with_context(|| {
                format!("Helm release manifest for revision {interrupted} is absent")
            })?;
        if crate::deployment_components::app_serves(target, &manifest).await? {
            return Ok(());
        }
    }
    let history = release_history(target, namespace, name).await?;
    match app_recovery_revision(&history, interrupted)
        .with_context(|| format!("recover Helm release {namespace}/{name}"))?
    {
        Some(revision) => rollback(target, namespace, name, Some(revision), wait).await,
        None => uninstall(target, namespace, name, wait).await,
    }
}

pub(crate) async fn rollback(
    target: &Target,
    namespace: &str,
    name: &str,
    revision: Option<u32>,
    wait: Wait,
) -> anyhow::Result<()> {
    let readiness = if let Wait::WatcherUntil { readiness, .. } = wait {
        let revision = revision.context("application rollback requires a Helm revision")?;
        let manifest = get(target, "manifest", name, revision)
            .await?
            .with_context(|| format!("Helm release manifest for revision {revision} is absent"))?;
        Some((readiness, manifest))
    } else {
        None
    };
    let operation = if revision.is_some() {
        "helm rollback"
    } else {
        "helm"
    };
    let revision_arg = revision.map(|revision| revision.to_string());
    let mut arguments = Vec::new();
    arguments.extend(revision_arg.as_deref().map(OsStr::new));
    arguments.push(OsStr::new("--server-side=true"));
    let (output, phase_end) =
        mutation_output(target, namespace, name, "rollback", &arguments, wait).await?;
    stdout(output, operation)?;
    if let Some((readiness, manifest)) = readiness {
        wait_app(
            target,
            name,
            &manifest,
            phase_end.min(Instant::now() + readiness),
        )
        .await?;
    }
    Ok(())
}

/// Uninstall a release with its owner's wait strategy.
pub async fn uninstall(
    target: &Target,
    namespace: &str,
    name: &str,
    wait: Wait,
) -> anyhow::Result<()> {
    let (output, _) = mutation_output(
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
    fn app_upgrade_requests_rollback_with_the_watcher() {
        let wait = Wait::WatcherUntil {
            deadline: Instant::now() + Duration::from_secs(120),
            readiness: Duration::from_secs(120),
        };
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
    fn app_upgrade_reserves_rollback_time_and_requires_a_minimum_budget() {
        let now = Instant::now();
        let timeout = |operation, remaining| {
            let wait = Wait::WatcherUntil {
                deadline: now + Duration::from_secs(remaining),
                readiness: Duration::from_secs(120),
            };
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
    fn app_recovery_restores_the_last_success_after_an_interrupted_rollback() {
        let history = [
            json!({"revision":1,"status":"superseded","description":"Install complete"}),
            json!({"revision":2,"status":"superseded","description":"Upgrade complete"}),
            json!({"revision":3,"status":"superseded","description":"Upgrade \"app\" failed: timeout"}),
            json!({"revision":4,"status":"failed","description":"Rollback \"app\" failed: update"}),
            json!({"revision":5,"status":"pending-rollback","description":"Rollback to 2"}),
        ];
        assert_eq!(app_recovery_revision(&history, 5).unwrap(), Some(2));
    }

    #[test]
    fn app_recovery_uninstalls_a_failed_first_install() {
        let first = json!({"revision":1,"status":"failed"});
        assert_eq!(app_recovery_revision(&[first], 1).unwrap(), None);
        let later = json!({"revision":2,"status":"failed"});
        assert!(app_recovery_revision(&[later], 2).is_err());
    }

    #[test]
    fn app_recovery_excludes_the_deployed_revision_that_does_not_serve() {
        let history = [
            json!({"revision":1,"status":"superseded","description":"Install complete"}),
            json!({"revision":2,"status":"superseded","description":"Rollback to 1"}),
            json!({"revision":3,"status":"failed","description":"Upgrade failed"}),
            json!({"revision":4,"status":"deployed","description":"Upgrade complete"}),
        ];
        assert_eq!(app_recovery_revision(&history, 4).unwrap(), Some(2));

        let first = json!({"revision":1,"status":"deployed","description":"Install complete"});
        assert_eq!(app_recovery_revision(&[first], 1).unwrap(), None);
    }
}
