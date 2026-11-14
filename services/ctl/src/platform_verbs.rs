//! Thin platform command adapters.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::{Args, Subcommand, ValueEnum};
use wamn_platform_install::platform::document::{ClusterDocument, UpgradeTarget};
use wamn_platform_install::platform::{deploy, provider, status};
use wamn_release_control::release_chart::Target;

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
    /// Read the current platform state without creating a cluster.
    Status(ShowArgs),
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
    mut output: impl Write,
) -> anyhow::Result<()> {
    let mut signals = signals()?;
    match command {
        ProductPlatformCommand::Deploy(args) => deploy(&args, &mut signals, output).await,
        ProductPlatformCommand::Status(args) => {
            let document = ClusterDocument::compile(&args.file)?;
            let directory = work_directory()?;
            let result = report_status(&document, &directory, &mut signals, &mut output).await;
            let cleanup = std::fs::remove_dir_all(directory).context("remove status directory");
            result.and(cleanup)
        }
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
    if std::env::var_os("WAMN_CONTROL_URL").is_some() {
        anyhow::ensure!(
            args.chart.is_none(),
            "control selects the prepared chart; --chart is only for bootstrap"
        );
        let selection = args
            .only
            .as_ref()
            .context("after bootstrap, select the existing Helm owner with --only")?;
        let control = crate::operation_verbs::ControlHost::from_env()?;
        return interruptible(signals, control.platform(serde_json::json!({"type":"update","document":document,"selection":match selection { UpgradeTarget::Control => "control".to_owned(), UpgradeTarget::Identity => "identity".to_owned(), UpgradeTarget::Host { release, group } => format!("host:{release}/{group}") }}))).await;
    }
    let discovery = work_directory()?;
    let existing = async {
        let kubeconfig = discovery.join("kubeconfig");
        let Some(context) = provider::available_kubeconfig(&document, &kubeconfig).await? else {
            return anyhow::Ok(false);
        };
        let target = Target {
            kubeconfig,
            context,
            namespace: document.org.clone(),
        };
        Ok(
            wamn_platform_install::platform::installed::InstalledPlatform::read(&target)
                .await?
                .is_some_and(|installed| installed.control()["enabled"] == true),
        )
    }
    .await;
    std::fs::remove_dir_all(&discovery)?;
    anyhow::ensure!(
        !existing?,
        "control already exists; set WAMN_CONTROL_URL and WAMN_TOKEN"
    );
    anyhow::ensure!(
        args.only.is_none(),
        "bootstrap does not update an existing Helm owner"
    );
    let directory = work_directory()?;
    writeln!(output, "work directory: {}", directory.display())?;
    output.flush()?;
    let result = interruptible(signals, async {
        deploy::deploy(&document, args.chart.as_deref(), &directory, &mut output).await
    })
    .await;
    let observed = report_status(&document, &directory, signals, &mut output).await;
    // A failed observation or output write must not replace the deployment error.
    // Only a newly created kind cluster mounts this run's registry directory.
    // Reapply repairs the original mount source, outside this work directory.
    let cleanup = if result.is_ok() && !directory.join("kubeconfig.registry").exists() {
        std::fs::remove_dir_all(directory).context("remove deployment directory")
    } else {
        Ok(())
    };
    result.and(observed).and(cleanup)
}

async fn report_status(
    document: &ClusterDocument,
    directory: &Path,
    signals: &mut Signals,
    output: &mut impl Write,
) -> anyhow::Result<()> {
    let observed = interruptible(signals, async {
        let kubeconfig = directory.join("status-kubeconfig");
        let context = provider::existing_kubeconfig(document, &kubeconfig).await?;
        let target = Target {
            kubeconfig,
            context,
            namespace: "platform".into(),
        };
        status::read(document, &target).await
    })
    .await;
    match observed {
        Ok(observed) => {
            status::write(&observed, output)?;
            output.flush()?;
            anyhow::ensure!(
                observed.category == status::Category::Ready,
                "platform is {}",
                observed.category
            );
            Ok(())
        }
        Err(error) => {
            // Keep diagnostics free of provider output, which can contain credentials.
            let _ = writeln!(
                output,
                "platform: unknown\nnext: restore access to the existing cluster and retry platform status"
            );
            let _ = output.flush();
            Err(error)
        }
    }
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

fn work_directory() -> anyhow::Result<PathBuf> {
    let directory = std::env::temp_dir().join(format!(
        "wamn-deploy-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder
        .create(&directory)
        .context("create private deployment directory")?;
    Ok(directory)
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

#[cfg(test)]
mod tests {
    use super::{ClusterDocument, show};
    #[derive(clap::Parser)]
    struct Cli {
        #[command(subcommand)]
        command: Command,
    }
    #[derive(clap::Subcommand)]
    enum Command {
        #[command(subcommand)]
        Platform(super::PlatformCommand),
    }
    use clap::Parser as _;
    use std::path::Path;

    fn shown(file: &Path) -> Vec<u8> {
        let cli = Cli::try_parse_from([
            "wamn-ctl".as_ref(),
            "platform".as_ref(),
            "show".as_ref(),
            file.as_os_str(),
        ])
        .expect("parse platform show");
        let Command::Platform(command) = cli.command;
        let mut output = Vec::new();
        let super::PlatformCommand::Show(args) = command;
        show(&args, &mut output).expect("show the cluster document");
        output
    }

    #[test]
    fn show_preserves_the_cluster_configuration() {
        let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/gcp/cluster.k");
        let document =
            ClusterDocument::compile(&committed).expect("compile the committed document");
        let output = shown(&committed);
        let directory =
            std::env::temp_dir().join(format!("wamn-platform-show-{}", std::process::id()));
        std::fs::create_dir(&directory).expect("create the directory");
        let file = directory.join("cluster.k");
        std::fs::write(&file, &output).expect("write show's output");
        let compiled = ClusterDocument::compile(&file).expect("compile show's output");
        std::fs::remove_dir_all(&directory).expect("remove the directory");

        assert_eq!(compiled, document);
    }
}
