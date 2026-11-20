//! Thin platform command adapters.

use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context as _;
use clap::{Args, Subcommand, ValueEnum};
use wamn_kubernetes::{Target, helm, job};
use wamn_platform_install::release_chart::bundle;

const INFRASTRUCTURE_RELEASE: &str = "wamn-infrastructure";
const PLATFORM_RELEASE: &str = "wamn";
const SYSTEM_SQL_JOB: &str = "wamn-system-sql";
const ROLLOUT_TIMEOUT: Duration = Duration::from_secs(960);

/// Platform verbs exposed by the product command.
#[derive(Debug, Subcommand)]
pub enum ProductPlatformCommand {
    /// Install or upgrade the shared Kubernetes platform.
    Deploy(DeployArgs),
    /// Inspect or apply project platform SQL through control.
    #[command(subcommand)]
    Schema(PlatformSchemaCommand),
}

#[derive(Debug, Args)]
pub struct DeployArgs {
    /// Helm values for this installation.
    values: PathBuf,
    /// Prepared chart bundle directory or OCI digest.
    #[arg(long, env = "WAMN_RELEASE_CHART")]
    chart: PathBuf,
    /// Install or upgrade only the selected chart.
    #[arg(long, value_enum)]
    only: Option<DeployChart>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum DeployChart {
    Infrastructure,
    Platform,
}

#[derive(Debug, Subcommand)]
pub enum PlatformSchemaCommand {
    /// Read initialization and migration state without changing SQL.
    Diff(PlatformSchemaArgs),
    /// Apply pending platform SQL without changing service releases.
    Apply(PlatformSchemaArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum PlatformSchemaTarget {
    Project,
}

#[derive(Debug, Args)]
pub struct PlatformSchemaArgs {
    /// The project database target.
    #[arg(long, value_enum)]
    target: PlatformSchemaTarget,
    /// The environment document.
    #[arg(long)]
    env: PathBuf,
}

pub async fn run_product(command: ProductPlatformCommand) -> anyhow::Result<()> {
    let mut signals = signals()?;
    let command = match command {
        ProductPlatformCommand::Deploy(args) => {
            return interruptible(&mut signals, deploy(&args)).await;
        }
        ProductPlatformCommand::Schema(command) => command,
    };
    let (args, apply) = match command {
        PlatformSchemaCommand::Diff(args) => (args, false),
        PlatformSchemaCommand::Apply(args) => (args, true),
    };
    let target = match args.target {
        PlatformSchemaTarget::Project => "project",
    };
    let document = wamn_environment::environment::document::EnvironmentFile::compile(&args.env)?;
    let projects: Vec<_> = document
        .projects
        .into_iter()
        .map(|project| project.name)
        .collect();
    let control = crate::operation_verbs::ControlHost::from_env()?;
    interruptible(
        &mut signals,
        control.platform_schema(serde_json::json!({
            "target": target,
            "projects": projects,
            "org": document.org,
            "env": document.env,
            "apply": apply,
        })),
    )
    .await
}

async fn deploy(args: &DeployArgs) -> anyhow::Result<()> {
    let values = args
        .values
        .canonicalize()
        .context("resolve the Helm values file")?;
    let infrastructure = !matches!(args.only, Some(DeployChart::Platform));
    let platform = !matches!(args.only, Some(DeployChart::Infrastructure));
    let registry_egress = platform.then(|| registry_egress(&values)).transpose()?;
    let chart = bundle::select(&args.chart)?;
    let reference = args
        .chart
        .to_str()
        .context("chart reference must be UTF-8")?;
    let local_reference;
    let application_chart = if reference.starts_with("oci://") {
        reference
    } else {
        let path = chart.directory().join("chart.ref");
        local_reference = std::fs::read_to_string(&path).with_context(|| {
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
        reference
    };
    let target = Target::developer("platform").await?;
    let charts = chart.directory().join("platform");
    if infrastructure {
        upgrade(
            &target,
            &charts.join("infra"),
            INFRASTRUCTURE_RELEASE,
            &[&values],
            helm::Install::CreateNamespace,
        )
        .await?;
    }
    if let Some(registry_egress) = registry_egress {
        let chart_values = tempfile::NamedTempFile::new()
            .context("create a file for the resolved platform values")?;
        serde_json::to_writer(
            chart_values.as_file(),
            &serde_json::json!({
                "applicationChart": application_chart,
                "registryEgress": registry_egress,
            }),
        )
        .context("write the resolved platform values")?;
        let jobs = kube::Api::namespaced(target.client.client(), &target.namespace);
        job::remove_previous(&jobs, SYSTEM_SQL_JOB, ROLLOUT_TIMEOUT).await?;
        upgrade(
            &target,
            &charts.join("platform"),
            PLATFORM_RELEASE,
            &[&values, chart_values.path()],
            helm::Install::ExistingNamespace,
        )
        .await?;
    }
    Ok(())
}

fn registry_egress(path: &Path) -> anyhow::Result<serde_json::Value> {
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

struct Signals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
}

fn signals() -> anyhow::Result<Signals> {
    Ok(Signals {
        #[cfg(unix)]
        interrupt: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .context("listen for interruption")?,
        #[cfg(unix)]
        terminate: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .context("listen for termination")?,
    })
}

async fn interruptible<T>(
    signals: &mut Signals,
    operation: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    let interrupted = async {
        #[cfg(unix)]
        signals.interrupt.recv().await;
        #[cfg(not(unix))]
        tokio::signal::ctrl_c()
            .await
            .context("listen for interruption")?;
        Ok::<_, anyhow::Error>(())
    };
    let terminated = async {
        #[cfg(unix)]
        signals.terminate.recv().await;
        #[cfg(not(unix))]
        std::future::pending::<()>().await;
    };
    // Dropping the operation releases its kill-on-drop children and port-forwards.
    tokio::select! {
        biased;
        result = interrupted => {
            result?;
            anyhow::bail!("platform command interrupted")
        }
        () = terminated => anyhow::bail!("platform command terminated"),
        result = operation => result,
    }
}
