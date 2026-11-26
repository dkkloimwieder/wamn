//! Thin platform command adapters.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Args, Subcommand, ValueEnum};
use wamn_kubernetes::Target;
use wamn_platform_install::{
    platform_install::{app_chart_reference, registry_egress},
    release_chart::bundle,
};

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
    let document = wamn_env::env::document::EnvFile::compile(&args.env)?;
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
    let app_chart = app_chart_reference(&args.chart, chart.directory())?;
    let target = Target::developer("platform").await?;
    wamn_platform_install::install::install(
        &target,
        &chart.directory().join("platform"),
        &values,
        infrastructure,
        &app_chart,
        registry_egress,
    )
    .await
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
