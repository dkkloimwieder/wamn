//! Arguments and output of the `platform` verbs.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Args, Subcommand};
use wamn_control::platform::document::ClusterDocument;

/// One `platform` verb.
#[derive(Debug, Subcommand)]
pub enum PlatformCommand {
    /// Print a cluster.k file in its standard KCL form.
    Show(ShowArgs),
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// The cluster.k document.
    pub file: PathBuf,
}

pub fn run(command: PlatformCommand, mut output: impl Write) -> anyhow::Result<()> {
    match command {
        PlatformCommand::Show(args) => {
            let document = ClusterDocument::compile(&args.file)?;
            output
                .write_all(document.to_kcl().as_bytes())
                .context("write the cluster document")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ClusterDocument, run};
    use crate::{Cli, Command};
    use clap::Parser as _;
    use std::path::Path;

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
        run(command, &mut output).expect("show the cluster document");
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
