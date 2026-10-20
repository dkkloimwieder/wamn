//! Arguments and output of the `platform` verbs.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::{Args, Subcommand};
use wamn_control::platform::document::ClusterDocument;
use wamn_control::platform::steps::{install_steps, read_platform_version};

/// One `platform` verb.
#[derive(Debug, Subcommand)]
pub enum PlatformCommand {
    /// Print a cluster.k file in its standard KCL form.
    Show(ShowArgs),
    /// Install the platform, or print its plan with --dry-run.
    Apply(ApplyArgs),
}

#[derive(Debug, Args)]
pub struct ApplyArgs {
    /// The cluster.k document.
    pub file: PathBuf,
    /// Print the install plan without changing resources.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// The cluster.k document.
    pub file: PathBuf,
}

pub async fn run(command: PlatformCommand, mut output: impl Write) -> anyhow::Result<()> {
    if let PlatformCommand::Apply(args) = &command
        && !args.dry_run
    {
        let document = ClusterDocument::compile(&args.file)?;
        let version = read_platform_version(Path::new("deploy/platform/release/Chart.yaml"))?;
        let directory = wamn_control::platform::lock::work_directory()?;
        return wamn_control::platform::install::apply(
            &document,
            &version,
            Path::new("."),
            &args
                .file
                .parent()
                .unwrap_or(Path::new("."))
                .join("environments"),
            &directory,
            |line| {
                writeln!(output, "{line}")?;
                output.flush().context("write install progress")
            },
        )
        .await;
    }
    run_with_chart(
        command,
        Path::new("deploy/platform/release/Chart.yaml"),
        output,
    )
}

fn run_with_chart(
    command: PlatformCommand,
    chart: &Path,
    mut output: impl Write,
) -> anyhow::Result<()> {
    match command {
        PlatformCommand::Show(args) => {
            let document = ClusterDocument::compile(&args.file)?;
            output
                .write_all(document.to_kcl().as_bytes())
                .context("write the cluster document")?;
        }
        PlatformCommand::Apply(args) => {
            let document = ClusterDocument::compile(&args.file)?;
            let version = read_platform_version(chart)?;
            for step in install_steps(&document, &version) {
                writeln!(output, "{} {}", step.number, step.title)?;
                writeln!(output, "  runs: {}", step.runs)?;
                for (name, value) in step.inputs {
                    let mut lines = value.lines();
                    writeln!(output, "  {name}: {}", lines.next().unwrap_or_default())?;
                    for line in lines {
                        writeln!(output, "    {line}")?;
                    }
                }
                if step.owner_inputs.is_empty() {
                    writeln!(output, "  owner inputs: none")?;
                }
                for input in step.owner_inputs {
                    writeln!(output, "  owner input: {}", input.name)?;
                    for line in input.instructions.lines() {
                        writeln!(output, "    {line}")?;
                    }
                }
                writeln!(output)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ClusterDocument, install_steps, read_platform_version, run, run_with_chart};
    use crate::{Cli, Command};
    use clap::Parser as _;
    use std::collections::BTreeSet;
    use std::path::Path;
    use wamn_control::platform::steps::Runner;

    #[tokio::test]
    async fn apply_without_dry_run_reads_the_document_before_starting() {
        let cli = Cli::try_parse_from(["wamn-ctl", "platform", "apply", "/missing/cluster.k"])
            .expect("parse platform apply");
        let Command::Platform(command) = cli.command else {
            panic!("expected a platform command");
        };
        let mut output = Vec::new();
        let error = run(command, &mut output).await.unwrap_err();
        assert_eq!(error.to_string(), "read /missing/cluster.k");
        assert!(output.is_empty());
    }

    #[test]
    fn apply_dry_run_lists_every_step_once_with_its_runner_and_the_chart_version() {
        let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/gcp/cluster.k");
        let cli = Cli::try_parse_from([
            "wamn-ctl".as_ref(),
            "platform".as_ref(),
            "apply".as_ref(),
            committed.as_os_str(),
            "--dry-run".as_ref(),
        ])
        .expect("parse platform apply --dry-run");
        let Command::Platform(command) = cli.command else {
            panic!("expected a platform command");
        };
        let mut output = Vec::new();
        let chart =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/platform/release/Chart.yaml");
        run_with_chart(command, &chart, &mut output).expect("print the install plan");
        assert!(!output.is_empty());

        let document = ClusterDocument::compile(&committed).expect("compile the document");
        let version = read_platform_version(&chart).expect("read the chart version");
        let steps = install_steps(&document, &version);
        let numbers: Vec<_> = steps.iter().map(|step| step.number).collect();
        assert_eq!(
            numbers,
            [
                "4.1", "4.2", "4.3", "4.4", "4.4.1", "4.5", "4.6", "4.7", "4.8", "4.9", "4.10",
                "4.11", "4.12", "4.13", "4.14", "4.15", "4.15.1", "4.16", "4.17", "4.18", "4.19",
                "4.19.1", "4.20", "4.21",
            ],
            "the subsections of gcp.md section 4, in order"
        );
        for step in &steps {
            match step.runs {
                Runner::Verb(name) | Runner::Job(name) | Runner::Owner(name) => {
                    assert!(!name.is_empty(), "{} names its runner", step.number);
                }
            }
        }
        let mut named = BTreeSet::new();
        for input in steps.iter().flat_map(|step| &step.owner_inputs) {
            assert!(named.insert(&input.name), "{} is named once", input.name);
        }
        assert!(
            steps
                .iter()
                .flat_map(|step| &step.inputs)
                .any(|(_, value)| *value == version),
            "the plan uses the chart's version"
        );
    }

    #[test]
    fn apply_dry_run_names_an_unreadable_chart_before_printing() {
        let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/gcp/cluster.k");
        let chart = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/platform/release/Chart.yaml/missing");
        let command = super::PlatformCommand::Apply(super::ApplyArgs {
            file: committed,
            dry_run: true,
        });
        let mut output = Vec::new();
        let error = run_with_chart(command, &chart, &mut output).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("read platform chart {}", chart.display())
        );
        assert!(output.is_empty());
    }

    fn show(file: &Path) -> Vec<u8> {
        let cli = Cli::try_parse_from([
            "wamn-ctl".as_ref(),
            "platform".as_ref(),
            "show".as_ref(),
            file.as_os_str(),
        ])
        .expect("parse platform show");
        let Command::Platform(command) = cli.command else {
            panic!("expected a platform command");
        };
        let mut output = Vec::new();
        run_with_chart(
            command,
            Path::new("deploy/platform/release/Chart.yaml"),
            &mut output,
        )
        .expect("show the cluster document");
        output
    }

    #[test]
    fn show_preserves_the_committed_document_and_repeats_the_same_bytes() {
        let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/gcp/cluster.k");
        let document =
            ClusterDocument::compile(&committed).expect("compile the committed document");
        let output = show(&committed);
        let directory =
            std::env::temp_dir().join(format!("wamn-platform-show-{}", std::process::id()));
        std::fs::create_dir(&directory).expect("create the directory");
        let file = directory.join("cluster.k");
        std::fs::write(&file, &output).expect("write show's output");
        let compiled = ClusterDocument::compile(&file).expect("compile show's output");
        let repeated = show(&file);
        std::fs::remove_dir_all(&directory).expect("remove the directory");

        assert_eq!(compiled, document);
        assert_eq!(repeated, output);
    }
}
