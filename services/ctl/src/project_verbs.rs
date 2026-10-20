//! Render the project document for OpenTofu.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Args, Subcommand};
use wamn_control::platform::project::ProjectDocument;

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// Write main.tf.json for the cluster and pools declared in project.k.
    Render(RenderArgs),
}

#[derive(Debug, Args)]
pub struct RenderArgs {
    /// The project.k document.
    pub file: PathBuf,
    /// The existing GCS bucket that holds OpenTofu state.
    #[arg(long)]
    pub state_bucket: String,
    /// The object prefix for this project's state in the bucket.
    #[arg(long)]
    pub state_prefix: String,
    /// The directory that receives main.tf.json.
    pub directory: PathBuf,
}

pub fn run(command: ProjectCommand) -> anyhow::Result<()> {
    match command {
        ProjectCommand::Render(args) => {
            let project = ProjectDocument::compile(&args.file)?;
            let bytes = wamn_schema_generator::render_project_tofu(
                &project.canonical_json()?,
                &args.state_bucket,
                &args.state_prefix,
            )?;
            std::fs::create_dir_all(&args.directory)
                .with_context(|| format!("create {}", args.directory.display()))?;
            let file = args.directory.join("main.tf.json");
            std::fs::write(&file, bytes).with_context(|| format!("write {}", file.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cli, Command};
    use clap::Parser as _;
    use serde_json::{Value, json};
    use std::path::Path;

    #[test]
    fn render_parses_owner_inputs_and_writes_only_main_tf_json() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/gcp/project.k");
        let directory =
            std::env::temp_dir().join(format!("wamn-project-render-{}", std::process::id()));
        let cli = Cli::try_parse_from([
            "wamn-ctl".as_ref(),
            "project".as_ref(),
            "render".as_ref(),
            source.as_os_str(),
            "--state-bucket".as_ref(),
            "owner-state".as_ref(),
            "--state-prefix".as_ref(),
            "gcp/dev".as_ref(),
            directory.as_os_str(),
        ])
        .expect("parse project render");
        let Command::Project(command) = cli.command else {
            panic!("expected project render");
        };
        run(command).expect("render the project");
        let files: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(files, ["main.tf.json"]);
        let rendered: Value =
            serde_json::from_slice(&std::fs::read(directory.join("main.tf.json")).unwrap())
                .unwrap();
        assert_eq!(
            rendered["terraform"]["backend"]["gcs"],
            json!({"bucket": "owner-state", "prefix": "gcp/dev"})
        );
        assert_eq!(
            rendered["resource"]["google_container_cluster"]["cluster"]["name"],
            "wamn"
        );
        assert_eq!(
            rendered["resource"]["google_container_node_pool"]
                .as_object()
                .unwrap()
                .len(),
            3
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
