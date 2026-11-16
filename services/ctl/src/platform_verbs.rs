//! Thin platform command adapters.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Args, Subcommand, ValueEnum};
use wamn_platform_install::platform::document::{ClusterDocument, UpgradeTarget};

/// Management verbs also available through the low-level control tool.
#[derive(Debug, Subcommand)]
pub enum PlatformCommand {
    /// Print a cluster.k file in its standard KCL form.
    Show(ShowArgs),
}

/// Platform verbs exposed by the product command.
#[derive(Debug, Subcommand)]
pub enum ProductPlatformCommand {
    /// Deploy platform resources from a prepared chart.
    Deploy(DeployArgs),
    /// Inspect or apply platform SQL through control.
    #[command(subcommand)]
    Schema(PlatformSchemaCommand),
    #[command(flatten)]
    Manage(PlatformCommand),
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
    System,
    Project,
}

#[derive(Debug)]
pub struct PlatformSchemaArgs {
    pub target: PlatformSchemaTarget,
    pub env: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct PlatformSchemaInput {
    /// The database target.
    #[arg(long, value_enum)]
    target: PlatformSchemaTarget,
    /// The environment document, required only for project target.
    #[arg(long, required_if_eq("target", "project"))]
    env: Option<PathBuf>,
}

impl clap::FromArgMatches for PlatformSchemaArgs {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        let input = PlatformSchemaInput::from_arg_matches(matches)?;
        if matches!(input.target, PlatformSchemaTarget::System) && input.env.is_some() {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "--env is refused for system target",
            ));
        }
        Ok(Self {
            target: input.target,
            env: input.env,
        })
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl Args for PlatformSchemaArgs {
    fn augment_args(command: clap::Command) -> clap::Command {
        PlatformSchemaInput::augment_args(command)
    }

    fn augment_args_for_update(command: clap::Command) -> clap::Command {
        PlatformSchemaInput::augment_args_for_update(command)
    }
}

#[derive(Debug, Args)]
pub struct DeployArgs {
    /// The cluster.k document.
    pub file: PathBuf,
    /// The prepared chart directory for initial installation.
    #[arg(long)]
    pub chart: Option<PathBuf>,
    /// Select control, identity, or host:<release>/<group> for an existing platform.
    #[arg(long, value_name = "TARGET")]
    pub only: Option<UpgradeTarget>,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// The cluster.k document.
    pub file: PathBuf,
}

pub async fn run_product(
    command: ProductPlatformCommand,
    output: impl Write,
) -> anyhow::Result<()> {
    let mut signals = signals()?;
    match command {
        ProductPlatformCommand::Deploy(args) => deploy(&args, &mut signals, output).await,

        ProductPlatformCommand::Schema(command) => {
            let (args, apply) = match command {
                PlatformSchemaCommand::Diff(args) => (args, false),
                PlatformSchemaCommand::Apply(args) => (args, true),
            };
            let (target, project, env) = match args.target {
                PlatformSchemaTarget::System => ("system", None, None),
                PlatformSchemaTarget::Project => {
                    let file = args.env.context("--env is required for project SQL")?;
                    let document =
                        wamn_environment::environment::document::EnvironmentDocument::compile(
                            &file,
                        )?;
                    ("project", Some(document.project), Some(document.env))
                }
            };
            let control = crate::operation_verbs::ControlHost::from_env()?;
            interruptible(&mut signals, control.platform(serde_json::json!({"type":"schema","target":target,"project":project,"env":env,"apply":apply}))).await
        }
        ProductPlatformCommand::Manage(command) => run(command, output),
    }
}

async fn deploy(
    args: &DeployArgs,
    signals: &mut Signals,
    mut output: impl Write,
) -> anyhow::Result<()> {
    let file = args
        .file
        .canonicalize()
        .with_context(|| format!("resolve cluster {}", args.file.display()))?;
    let document = ClusterDocument::compile(&file)?;
    writeln!(output, "cluster: {}", file.display())?;

    anyhow::ensure!(
        args.chart.is_none(),
        "control selects the prepared chart; --chart is only for bootstrap"
    );
    let selection = args
        .only
        .as_ref()
        .context("after bootstrap, select the existing Helm owner with --only")?;
    let control = crate::operation_verbs::ControlHost::from_env()?;
    interruptible(signals, control.platform(serde_json::json!({"type":"update","document":document,"selection":match selection { UpgradeTarget::Control => "control".to_owned(), UpgradeTarget::Identity => "identity".to_owned(), UpgradeTarget::Host { release, group } => format!("host:{release}/{group}") }}))).await

}

struct Signals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
}

fn signals() -> anyhow::Result<Signals> {
    // Register before synchronous KCL compilation, and retain signals between operations.
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

pub fn run(command: PlatformCommand, output: impl Write) -> anyhow::Result<()> {
    let PlatformCommand::Show(args) = command;
    show(&args, output)
}

fn show(args: &ShowArgs, mut output: impl Write) -> anyhow::Result<()> {
    let document = ClusterDocument::compile(&args.file)?;
    output
        .write_all(document.to_kcl().as_bytes())
        .context("write the cluster document")
}
