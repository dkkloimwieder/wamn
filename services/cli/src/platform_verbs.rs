//! Thin platform command adapters.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context as _;
use clap::{Args, Subcommand, ValueEnum};
use wamn_kubernetes::{Target, helm, job, kubectl};
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
    let chart = bundle::select(&args.chart)?;
    let kubeconfig = PathBuf::new();
    let context = kubectl::current_context(&kubeconfig)?;
    let target = Target {
        kubeconfig,
        context,
        namespace: "platform".into(),
    };
    let infrastructure = !matches!(args.only, Some(DeployChart::Platform));
    let platform = !matches!(args.only, Some(DeployChart::Infrastructure));
    let charts = chart.directory().join("platform");
    if infrastructure {
        upgrade(
            &target,
            &charts.join("infra"),
            INFRASTRUCTURE_RELEASE,
            &values,
            helm::Install::CreateNamespace,
        )
        .await?;
        // The platform chart needs the CloudNativePG admission webhook.
        kubectl::wait_deployment_ready(&target, "cnpg-controller-manager", ROLLOUT_TIMEOUT).await?;
    }
    if platform {
        let deadline = tokio::time::Instant::now() + ROLLOUT_TIMEOUT;
        job::clear_finished(&target, SYSTEM_SQL_JOB, deadline).await?;
        upgrade(
            &target,
            &charts.join("platform"),
            PLATFORM_RELEASE,
            &values,
            helm::Install::ExistingNamespace,
        )
        .await?;
    }
    let deadline = tokio::time::Instant::now() + ROLLOUT_TIMEOUT;
    let wait = async |namespace: &str, command: &[&str]| {
        kubectl::with_timeout(
            deadline,
            &target.kubeconfig,
            &target.context,
            namespace,
            command,
        )
        .await
    };
    let infrastructure_rollout = async {
        if infrastructure {
            // The Traefik chart appends the release namespace to its instance label.
            let selector = format!(
                "app.kubernetes.io/instance in ({INFRASTRUCTURE_RELEASE},{INFRASTRUCTURE_RELEASE}-{})",
                target.namespace
            );
            let command = ["rollout", "status", "deployment", "--selector", &selector];
            tokio::try_join!(
                wait("cert-manager", &command),
                wait(&target.namespace, &command)
            )
            .context("wait for infrastructure operators")?;
        }
        anyhow::Ok(())
    };
    let platform_rollout = async {
        if platform {
            let selector = format!("app.kubernetes.io/instance in ({PLATFORM_RELEASE},identity)");
            let rollout = [
                "rollout",
                "status",
                "deployment,statefulset",
                "--selector",
                &selector,
            ];
            tokio::try_join!(
                wait(&target.namespace, &rollout),
                wait("hosts", &["rollout", "status", "deployment/wamn-edge"]),
                job::wait(
                    &target,
                    SYSTEM_SQL_JOB,
                    deadline.saturating_duration_since(tokio::time::Instant::now())
                ),
            )
            .context("wait for platform rollouts and the system SQL Job")?;
        }
        anyhow::Ok(())
    };
    tokio::try_join!(infrastructure_rollout, platform_rollout)?;
    Ok(())
}

async fn upgrade(
    target: &Target,
    chart: &Path,
    release: &str,
    values: &Path,
    install: helm::Install,
) -> anyhow::Result<()> {
    helm::upgrade(
        target,
        &target.namespace,
        chart,
        release,
        values,
        helm::Wait::None,
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
