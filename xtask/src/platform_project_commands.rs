//! Render the project document for OpenTofu.

use std::path::PathBuf;

use super::platform_project::ProjectDocument;
use anyhow::Context as _;
use clap::{Args, Subcommand};

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

pub fn project(command: ProjectCommand) -> anyhow::Result<()> {
    match command {
        ProjectCommand::Render(args) => {
            let project = ProjectDocument::compile(&args.file)?;
            let bytes = super::platform_tofu::render_project_tofu(
                &project,
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
