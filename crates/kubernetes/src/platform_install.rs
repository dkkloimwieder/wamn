//! Helm installation of the shared platform.

use std::{net::IpAddr, path::Path, time::Duration};

use anyhow::Context as _;
use serde_json::Value;
use wamn_platform_install::install::PlatformInstallation;

use crate::{Target, helm, job};

const INFRASTRUCTURE_RELEASE: &str = "wamn-infrastructure";
const PLATFORM_RELEASE: &str = "wamn";
const SYSTEM_SQL_JOB: &str = "wamn-system-sql";
const ROLLOUT_TIMEOUT: Duration = Duration::from_mins(16);

impl PlatformInstallation for Target {
    async fn install_infrastructure(&self, charts: &Path, values: &Path) -> anyhow::Result<()> {
        upgrade(
            self,
            &charts.join("infra"),
            INFRASTRUCTURE_RELEASE,
            &[values],
            helm::Install::CreateNamespace,
        )
        .await
    }

    async fn install_platform(
        &self,
        charts: &Path,
        values: &Path,
        overrides: &Value,
    ) -> anyhow::Result<()> {
        let chart_values = tempfile::NamedTempFile::new()
            .context("create a file for the resolved platform values")?;
        serde_json::to_writer(chart_values.as_file(), overrides)
            .context("write the resolved platform values")?;
        let jobs = kube::Api::namespaced(self.client.client(), &self.namespace);
        job::remove_previous(&jobs, SYSTEM_SQL_JOB, ROLLOUT_TIMEOUT).await?;
        upgrade(
            self,
            &charts.join("platform"),
            PLATFORM_RELEASE,
            &[values, chart_values.path()],
            helm::Install::ExistingNamespace,
        )
        .await
    }
}

/// Resolve the published application chart reference used by the platform.
pub fn application_chart_reference(
    chart_reference: &Path,
    chart_directory: &Path,
) -> anyhow::Result<String> {
    let reference = chart_reference
        .to_str()
        .context("chart reference must be UTF-8")?;
    if reference.starts_with("oci://") {
        Ok(reference.into())
    } else {
        let path = chart_directory.join("chart.ref");
        let local_reference = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "read {} from a bundle built by cargo xtask build-platform",
                path.display()
            )
        })?;
        let reference = local_reference.trim();
        let (_, digest) = reference
            .strip_prefix("oci://")
            .and_then(|reference| reference.rsplit_once('@'))
            .filter(|(repository, _)| !repository.is_empty())
            .with_context(|| {
                format!(
                    "{} must contain an OCI chart reference pinned by digest",
                    path.display()
                )
            })?;
        digest
            .parse::<wamn_digest::Digest>()
            .with_context(|| format!("invalid OCI chart digest in {}", path.display()))?;
        Ok(reference.into())
    }
}

/// Read the explicit registry destinations or derive them from the registry endpoint.
pub fn registry_egress(path: &Path) -> anyhow::Result<serde_json::Value> {
    let read_context = || format!("read platform values from {}", path.display());
    let file = std::fs::File::open(path).with_context(read_context)?;
    let mut values: serde_yaml::Value = serde_yaml::from_reader(file).with_context(read_context)?;
    values.apply_merge().with_context(read_context)?;
    let egress = &values["registryEgress"];
    if !egress.is_null() {
        let destinations = egress
            .as_sequence()
            .context("registryEgress must be a list of CIDR and TCP port pairs")?;
        if !destinations.is_empty() {
            return serde_json::to_value(destinations).context("encode registryEgress");
        }
    }

    let endpoint = &values["registryEndpoint"];
    let address: IpAddr = endpoint["address"]
        .as_str()
        .context("set registryEgress or supply an IP address in registryEndpoint.address")?
        .parse()
        .context("registryEndpoint.address must be an IP address to derive registryEgress")?;
    let port = endpoint["port"]
        .as_u64()
        .filter(|port| (1..=u64::from(u16::MAX)).contains(port))
        .context("registryEndpoint.port must be a valid TCP port to derive registryEgress")?;
    let prefix = if address.is_ipv4() { 32 } else { 128 };
    Ok(serde_json::json!([{"cidr": format!("{address}/{prefix}"), "port": port}]))
}

async fn upgrade(
    target: &Target,
    chart: &Path,
    release: &str,
    values: &[&Path],
    install: helm::Install,
) -> anyhow::Result<()> {
    helm::upgrade(
        target,
        &target.namespace,
        chart,
        release,
        values,
        helm::Wait::Platform {
            timeout: ROLLOUT_TIMEOUT,
            jobs: release == PLATFORM_RELEASE,
        },
        install,
    )
    .await
    .with_context(|| format!("install or upgrade {release}"))
}
