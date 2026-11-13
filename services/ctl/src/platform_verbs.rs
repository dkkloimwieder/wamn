//! Thin platform command adapters.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::{Args, Subcommand, ValueEnum};
use wamn_control_provision::schema_migrations::MigrationDatabase;
use wamn_environment_control::platform::document::{ClusterDocument, UpgradeTarget};
use wamn_environment_control::platform::pools::Direction;
use wamn_environment_control::platform::{artifact_plan, deploy, provider, status};
use wamn_environment_control::platform_migrations;
use wamn_package_control::migrations::MigrationResolution;
use wamn_release_control::release_chart::Target;

/// Management verbs also available through the low-level control tool.
#[derive(Debug, Subcommand)]
pub enum PlatformCommand {
    /// Print a cluster.k file in its standard KCL form.
    Show(ShowArgs),
    /// Set every node pool to its declared size and wait for PostgreSQL and NATS.
    Wake(PoolsArgs),
    /// Set every node pool to 0 nodes.
    Sleep(PoolsArgs),
}

/// Platform verbs exposed by the product command.
#[derive(Debug, Subcommand)]
pub enum ProductPlatformCommand {
    /// Deploy platform resources from a prepared chart.
    Deploy(DeployArgs),
    /// Read the current platform state without creating a cluster.
    Status(ShowArgs),
    /// Inspect or apply platform SQL from an explicitly selected prepared chart.
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
    Apply(PlatformSchemaApplyArgs),
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
    pub chart: PathBuf,
}

#[derive(Debug, Args)]
struct PlatformSchemaInput {
    /// The database target.
    #[arg(long, value_enum)]
    target: PlatformSchemaTarget,
    /// The environment document, required only for project target.
    #[arg(long, required_if_eq("target", "project"))]
    env: Option<PathBuf>,
    /// The prepared chart whose ctl image supplies the SQL.
    #[arg(long)]
    chart: PathBuf,
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
            chart: input.chart,
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
pub struct PlatformSchemaApplyArgs {
    #[command(flatten)]
    pub selection: PlatformSchemaArgs,
    /// Resolve one uncertain nontransactional file after inspection.
    #[arg(long, value_name = "PATH=applied|retry", value_parser = platform_resolution)]
    pub resolve: Option<MigrationResolution>,
}

pub(crate) fn platform_resolution(value: &str) -> anyhow::Result<MigrationResolution> {
    anyhow::ensure!(
        !value.contains(':'),
        "resolution must be <path>=applied|retry"
    );
    let target = if value.starts_with("migrations/project/") {
        "project"
    } else {
        "system"
    };
    format!("{target}:{value}").parse()
}

/// In-image entry points. Both actions read the same embedded migration bytes.
#[derive(Debug, Subcommand)]
pub enum SystemSchemaCommand {
    Diff(SystemSchemaArgs),
    Apply(SystemSchemaApplyArgs),
}

#[derive(Debug, Args)]
pub struct SystemSchemaArgs {
    /// The system database administrator URL supplied by the Job Secret.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL", hide_env_values = true)]
    pub system_url: String,
    /// The immutable ctl image selected by the prepared chart.
    #[arg(long)]
    pub image: String,
}

#[derive(Debug, Args)]
pub struct SystemSchemaApplyArgs {
    #[command(flatten)]
    pub database: SystemSchemaArgs,
    #[arg(long, value_name = "PATH=applied|retry", value_parser = platform_resolution)]
    pub resolve: Option<MigrationResolution>,
}

pub async fn run_system_schema(
    command: SystemSchemaCommand,
    mut output: impl Write,
) -> anyhow::Result<()> {
    let (args, apply, resolution) = match &command {
        SystemSchemaCommand::Diff(args) => (args, false, None),
        SystemSchemaCommand::Apply(args) => (&args.database, true, args.resolve.as_ref()),
    };
    writeln!(output, "system SQL image: {}", args.image)?;
    output.flush()?;
    let (mut client, connection) = tokio_postgres::connect(&args.system_url, tokio_postgres::NoTls)
        .await
        .context("connect to the system SQL database")?;
    let connection = tokio::spawn(connection);
    let result = async {
        client.batch_execute("SET ROLE wamn_system").await?;
        let sources = platform_migrations::embedded_sources(MigrationDatabase::System);
        platform_migrations::run(
            &mut client,
            platform_migrations::MigrationTarget::System,
            &sources,
            apply,
            resolution,
            &mut output,
        )
        .await?;
        anyhow::Ok(())
    }
    .await;
    drop(client);
    let driven = connection.await.context("join the system SQL connection")?;
    result.and(driven.context("drive the system SQL connection"))
}

#[derive(Debug, Args)]
pub struct DeployArgs {
    /// The cluster.k document.
    pub file: PathBuf,
    /// The prepared chart directory, required for installation and --only.
    #[arg(long)]
    pub chart: Option<PathBuf>,
    /// Select control, identity, or host:<release>/<group> for an existing platform.
    #[arg(long, value_name = "TARGET")]
    pub only: Option<UpgradeTarget>,
}

#[derive(Debug, Args)]
pub struct PoolsArgs {
    /// The cluster.k document. Its pools come from project.k in the same directory.
    pub file: PathBuf,
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
            let (args, apply, resolution) = match &command {
                PlatformSchemaCommand::Diff(args) => (args, false, None),
                PlatformSchemaCommand::Apply(args) => {
                    (&args.selection, true, args.resolve.as_ref())
                }
            };
            let chart = artifact_plan::select(&args.chart)?;
            let target =
                wamn_environment_control::environment::kubernetes_target("platform".into())?;
            let label = match args.target {
                PlatformSchemaTarget::System => "system",
                PlatformSchemaTarget::Project => "project",
            };
            writeln!(
                output,
                "{label} SQL target: {}/{}",
                target.context, target.namespace
            )?;
            writeln!(output, "selected chart: {}", chart.directory().display())?;
            writeln!(output, "{label} SQL image: {}", chart.image("ctl")?)?;
            output.flush()?;
            let directory = work_directory()?;
            let result = Box::pin(interruptible(&mut signals, async {
                match args.target {
                    PlatformSchemaTarget::System => {
                        let log = wamn_environment_control::platform::system_schema(
                            &target, &directory, &chart, apply, resolution).await?;
                        writeln!(output, "{log}")?;
                    }
                    PlatformSchemaTarget::Project => {
                        let file = args.env.as_deref().context("--env is required for project target")?;
                        let mut platform = wamn_environment_control::environment::Platform::project_schema_from_env()?;
                        platform.target = target;
                        let document = wamn_environment_control::environment::document::EnvironmentDocument::read_deployed(&platform, file, None).await?;
                        writeln!(output, "project SQL environment: {}", document.triple())?;
                        output.flush()?;
                        wamn_environment_control::environment::sql::project_schema_deployed(
                            &platform, &document, chart.image("ctl")?, apply, resolution, &directory).await?;
                    }
                }
                output.flush().context("write the platform SQL result")
            })).await;
            // Retain failed Job logs, including committed SQL and the resume command.
            if result.is_ok() {
                std::fs::remove_dir_all(directory).context("remove platform SQL directory")?;
            }
            result
        }
        ProductPlatformCommand::Manage(command) => {
            interruptible(&mut signals, run(command, output)).await
        }
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
    let directory = work_directory()?;
    writeln!(output, "work directory: {}", directory.display())?;
    output.flush()?;
    let result = interruptible(signals, async {
        match &args.only {
            Some(selection) => {
                let chart = artifact_plan::select(
                    args.chart
                        .as_deref()
                        .context("--chart is required with --only. No resources changed.")?,
                )?;
                let (name, namespace) = selection.owner(&document);
                writeln!(
                    output,
                    "selected chart ({namespace}/{name}): {}",
                    chart.directory().display()
                )?;
                output.flush()?;
                let kubeconfig = directory.join("kubeconfig");
                let context = provider::existing_kubeconfig(&document, &kubeconfig).await?;
                let target = Target {
                    kubeconfig,
                    context,
                    namespace: "platform".into(),
                };
                let result =
                    deploy::update(&document, &chart, &target, selection, &directory).await;
                let observed = status::read_target(&document, &target, selection).await;
                let reported = (|| -> anyhow::Result<()> {
                    status::write_target(&observed, &mut output)?;
                    output.flush()?;
                    anyhow::ensure!(
                        observed.category == status::Category::Ready,
                        "target {} is {}",
                        observed.name,
                        observed.category
                    );
                    Ok(())
                })();
                result.and(reported)
            }
            None => deploy::deploy(&document, args.chart.as_deref(), &directory, &mut output).await,
        }
    })
    .await;
    if args.only.is_some() {
        let cleanup = std::fs::remove_dir_all(directory).context("remove update directory");
        return result.and(cleanup);
    }
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

pub async fn run(command: PlatformCommand, mut output: impl Write) -> anyhow::Result<()> {
    if let PlatformCommand::Wake(args) | PlatformCommand::Sleep(args) = &command {
        let direction = match &command {
            PlatformCommand::Wake(_) => Direction::Wake,
            _ => Direction::Sleep,
        };
        let document = ClusterDocument::compile(&args.file)?;
        return wamn_environment_control::platform::pools::resize(
            &document,
            &args
                .file
                .parent()
                .unwrap_or(Path::new("."))
                .join("project.k"),
            direction,
            &wamn_environment_control::platform::pools::work_directory()?,
            |line| {
                writeln!(output, "{line}")?;
                output.flush().context("write resize progress")
            },
        )
        .await;
    }
    let PlatformCommand::Show(args) = command else {
        unreachable!()
    };
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
        let super::PlatformCommand::Show(args) = command else {
            panic!("expected show");
        };
        show(&args, &mut output).expect("show the cluster document");
        output
    }

    #[test]
    fn show_preserves_the_committed_document_and_repeats_the_same_bytes() {
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
        let repeated = shown(&file);
        std::fs::remove_dir_all(&directory).expect("remove the directory");

        assert_eq!(compiled, document);
        assert_eq!(repeated, output);
    }
}
