//! Thin platform command adapters.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::{Args, Subcommand};
use wamn_environment_control::platform::document::ClusterDocument;
use wamn_environment_control::platform::pools::Direction;
use wamn_environment_control::platform::{artifact_plan, deploy, provider, status};
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
    #[command(flatten)]
    Manage(PlatformCommand),
}

#[derive(Debug, Args)]
pub struct DeployArgs {
    /// The cluster.k document.
    pub file: PathBuf,
    /// The prepared platform chart directory.
    #[arg(long)]
    pub chart: PathBuf,
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
    let chart = artifact_plan::select(&args.chart)?;
    writeln!(output, "cluster: {}", file.display())?;
    writeln!(output, "chart: {}", chart.directory().display())?;
    // Kind nodes can mount files from this directory. Retain it for their lifetime.
    let directory = work_directory()?;
    writeln!(output, "work directory: {}", directory.display())?;
    output.flush()?;
    let result = interruptible(signals, async {
        let kubeconfig = directory.join("kubeconfig");
        let context = document.provider.kubeconfig(&document, &kubeconfig).await?;
        let target = Target {
            kubeconfig,
            context,
            namespace: "platform".into(),
        };
        deploy::deploy(&document, &chart, &target, &directory, &mut output).await
    })
    .await;
    let observed = report_status(&document, &directory, signals, &mut output).await;
    // A failed observation or output write must not replace the deployment error.
    result.and(observed)
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
