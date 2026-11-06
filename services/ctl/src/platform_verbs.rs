//! Thin platform command adapters.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::{Args, Subcommand};
use wamn_environment_control::platform::document::ClusterDocument;
use wamn_environment_control::platform::pools::Direction;
use wamn_environment_control::platform::{artifact_plan, deploy};
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
    let ProductPlatformCommand::Deploy(args) = command else {
        let ProductPlatformCommand::Manage(command) = command else {
            unreachable!()
        };
        return run(command, output).await;
    };
    let file = args
        .file
        .canonicalize()
        .with_context(|| format!("resolve cluster {}", args.file.display()))?;
    let document = ClusterDocument::compile(&file)?;
    let chart = artifact_plan::select(&args.chart)?;
    writeln!(output, "cluster: {}", file.display())?;
    writeln!(output, "chart: {}", chart.directory().display())?;
    // The provider can mount files below this directory into kind nodes. Keep
    // the invocation's directory for the lifetime of those nodes.
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
    writeln!(output, "work directory: {}", directory.display())?;
    output.flush()?;
    let kubeconfig = directory.join("kubeconfig");
    let context = document.provider.kubeconfig(&document, &kubeconfig).await?;
    let target = Target {
        kubeconfig,
        context,
        namespace: "platform".into(),
    };
    deploy::deploy(&document, &chart, &target, &directory, &mut output).await
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
    use super::{ClusterDocument, run, show};
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

    #[test]
    fn control_tool_has_no_platform_apply_or_deploy() {
        for verb in ["apply", "deploy"] {
            assert!(Cli::try_parse_from(["wamn-ctl", "platform", verb, "cluster.k"]).is_err());
        }
    }

    #[tokio::test]
    async fn wake_and_sleep_refuse_a_kind_document_before_reading_project_k() {
        let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/gcp/cluster.k");
        let source = std::fs::read_to_string(&committed).expect("read the committed document");
        let directory =
            std::env::temp_dir().join(format!("wamn-platform-pools-{}", std::process::id()));
        std::fs::create_dir(&directory).expect("create the directory");
        let file = directory.join("cluster.k");
        std::fs::write(
            &file,
            source.replace(r#"provider = "gcp""#, r#"provider = "kind""#),
        )
        .expect("write the kind document");
        let mut errors = Vec::new();
        for verb in ["wake", "sleep"] {
            let cli = Cli::try_parse_from([
                "wamn-ctl".as_ref(),
                "platform".as_ref(),
                verb.as_ref(),
                file.as_os_str(),
            ])
            .expect("parse the platform verb");
            let Command::Platform(command) = cli.command;
            let mut output = Vec::new();
            errors.push(run(command, &mut output).await.unwrap_err().to_string());
            assert!(output.is_empty());
        }
        std::fs::remove_dir_all(&directory).expect("remove the directory");
        for error in errors {
            assert_eq!(
                error,
                "platform wake and sleep resize GCP node pools, and cluster wamn is a kind cluster"
            );
        }
    }

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
