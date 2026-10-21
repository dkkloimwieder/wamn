//! Shared cloud or local chart inputs and role transfers for the kind cases.
use anyhow::{Context as _, ensure};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use wamn_control::platform::{document::ClusterDocument, steps};
use wash_runtime::oci::{OciConfig, OciPullPolicy, pull_component, push_component};

pub fn source_chart(repository: &Path, directory: &Path) -> anyhow::Result<PathBuf> {
    if let Some(chart) = std::env::var_os("WAMN_RELEASE_CHART") {
        return Ok(chart.into());
    }
    let version =
        steps::read_platform_version(&repository.join("deploy/platform/release/Chart.yaml"))?;
    let source_registry =
        ClusterDocument::compile(&repository.join("deploy/gcp/cluster.k"))?.registry;
    let agent = Path::new(&std::env::var_os("HOME").context("HOME is required")?)
        .join(".config/gcloud-wamn-agent");
    ensure!(
        std::env::var_os("CLOUDSDK_CONFIG").as_deref() == Some(agent.as_os_str()),
        "export CLOUDSDK_CONFIG={} for the agent identity",
        agent.display()
    );
    let docker_config = std::env::var_os("DOCKER_CONFIG")
        .context("export DOCKER_CONFIG with the GCP gcloud credential helper")?;
    let credentials: Value = serde_json::from_slice(&std::fs::read(
        Path::new(&docker_config).join("config.json"),
    )?)?;
    let host = source_registry.split('/').next().context("registry host")?;
    ensure!(
        credentials["credHelpers"][host] == "gcloud",
        "DOCKER_CONFIG must select the gcloud credential helper for {host}"
    );
    let chart = directory.join("source-chart");
    stamp_registry(repository, &source_registry, &version, &chart)?;
    Ok(chart)
}

pub fn stamp_registry(
    repository: &Path,
    registry: &str,
    version: &str,
    out: &Path,
) -> anyhow::Result<()> {
    let target = std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into());
    let output = Command::new(repository.join(target).join("debug/wamn-ctl"))
        .current_dir(repository)
        .args([
            "build-platform",
            "--stamp-only",
            "--registry",
            registry,
            "--out",
        ])
        .arg(out)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("stamp registry images of platform {version}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprint!("{stderr}");
    if !output.status.success() {
        ensure!(
            stderr.contains(version),
            "stamp-only error does not name version {version}: {stderr}"
        );
        anyhow::bail!("stamp registry images of platform {version}: {stderr}");
    }
    Ok(())
}

pub async fn copy_role(from: &str, to: &str, destination: OciConfig) -> anyhow::Result<String> {
    let insecure = matches!(
        from.split([':', '/']).next(),
        Some("localhost" | "127.0.0.1")
    );
    let (bytes, _) = pull_component(
        from,
        OciConfig {
            insecure,
            ..Default::default()
        },
        OciPullPolicy::Always,
    )
    .await
    .with_context(|| format!("pull {from}"))?;
    push_component(to, &bytes, destination, None)
        .await
        .with_context(|| format!("push {to}"))
}
