//! Web client files in a bucket (docs/plan/web-deployment.md).
//!
//! The verb builds the client with pnpm. `wamn_control::web_upload` writes it.

use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use clap::{Args, Subcommand};
use tokio::process::Command;
use wamn_control::web_scope::{SCOPE_FILE, scope_file_bytes};
use wamn_control::web_upload::{
    ExistingObject, Sink, client_project, client_root, collect, release_hex, require_head,
    write_files,
};

/// Web client commands.
#[derive(Debug, Args)]
pub struct WebArgs {
    #[command(subcommand)]
    pub command: WebCommand,
}

#[derive(Debug, Subcommand)]
pub enum WebCommand {
    /// Build an application's web client and write it to a bucket.
    Upload(UploadArgs),
}

#[derive(Debug, Args)]
pub struct UploadArgs {
    /// The application directory, which holds `wamn.json` and `web/`.
    pub app: PathBuf,
    /// The release the files belong to: the manifest digest, `sha256:` and 64
    /// lowercase hexadecimal digits.
    #[arg(long)]
    pub release: String,
    /// The bucket and an optional prefix, `s3://<bucket>[/<prefix>]` or
    /// `gs://<bucket>[/<prefix>]`. For `s3://` the `AWS_*` variables supply
    /// the endpoint and the credentials. For `gs://` the `GOOGLE_*` variables
    /// and Application Default Credentials do.
    #[arg(long)]
    pub bucket: String,
    /// The org the client signs in to. The project comes from the client
    /// package name, `@wamn/<project>-client`. Both go to `config.json`
    /// beside `index.html`, not into the build.
    #[arg(long)]
    pub org: String,
    /// The project-environment database that holds the release head. The
    /// upload refuses a release that is not the head, before the build.
    /// `WAMN_WEB_DATABASE_URL` keeps the password off the command line.
    #[arg(long, env = "WAMN_WEB_DATABASE_URL", hide_env_values = true)]
    pub database_url: String,
}

pub async fn run(args: WebArgs) -> anyhow::Result<()> {
    match args.command {
        WebCommand::Upload(args) => upload(args).await,
    }
}

async fn upload(args: UploadArgs) -> anyhow::Result<()> {
    let hex = release_hex(&args.release)?;
    require_head(&args.database_url, &args.release).await?;
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(wamn_schema_generator::package_manifest_path(&args.app)).with_context(
            || {
                format!(
                    "read {}",
                    wamn_schema_generator::package_manifest_path(&args.app).display()
                )
            },
        )?,
    )?;
    let package = manifest
        .pointer("/package/id")
        .and_then(serde_json::Value::as_str)
        .context("wamn.json names its package id")?;
    let client = manifest
        .pointer("/client_package/name")
        .and_then(serde_json::Value::as_str)
        .context("wamn.json names its client package")?;
    let project = client_project(client)?;
    let web = args.app.join("web");
    let status = Command::new("pnpm")
        .arg("--dir")
        .arg(&web)
        .args(["run", "build"])
        .status()
        .await
        .context("run pnpm; the web client builds with Vite through pnpm")?;
    ensure!(status.success(), "the web build failed: {status}");
    let dist = web.join("dist");
    let sink = Sink::parse(&args.bucket)?;
    let store = sink.store()?;
    let root = client_root(sink.prefix, package, hex);
    let mut files = Vec::new();
    collect(&dist, &mut files)?;
    ensure!(
        files.iter().any(|file| file == &dist.join("index.html")),
        "the build wrote no index.html"
    );
    let scope = scope_file_bytes(&args.org, project);
    write_files(
        store.as_ref(),
        &root,
        &dist,
        &mut files,
        &scope,
        ExistingObject::Refuse,
    )
    .await?;
    println!(
        "{}{}/{root}/ {} files and {SCOPE_FILE}",
        sink.scheme.prefix(),
        sink.bucket,
        files.len()
    );
    Ok(())
}
