//! Web client files in a bucket (docs/plan/web-deployment.md).
//!
//! `wamn_control::web_upload` writes the built client of each package of the
//! release that an environment serves, from its package artifact.

use clap::{Args, Subcommand};
use wamn_control::environment::Platform;
use wamn_control::web_upload::upload_release;

/// Web client commands.
#[derive(Debug, Args)]
pub struct WebArgs {
    #[command(subcommand)]
    pub command: WebCommand,
}

#[derive(Debug, Subcommand)]
pub enum WebCommand {
    /// Write the built web clients of the release an environment serves to a bucket.
    Upload(UploadArgs),
}

#[derive(Debug, Args)]
pub struct UploadArgs {
    /// The environment, `org/project/env`. Its release chart names the release
    /// it serves; the platform inputs of `wamn-ctl env` locate the chart and
    /// the package artifacts.
    pub coordinate: String,
    /// The release the files belong to: the manifest digest, `sha256:` and 64
    /// lowercase hexadecimal digits. The upload refuses a release that the
    /// environment does not serve.
    #[arg(long)]
    pub release: String,
    /// The bucket and an optional prefix, `s3://<bucket>[/<prefix>]` or
    /// `gs://<bucket>[/<prefix>]`. For `s3://` the `AWS_*` variables supply
    /// the endpoint and the credentials. For `gs://` the `GOOGLE_*` variables
    /// and Application Default Credentials do.
    #[arg(long)]
    pub bucket: String,
}

pub async fn run(args: WebArgs) -> anyhow::Result<()> {
    match args.command {
        WebCommand::Upload(args) => upload(args).await,
    }
}

async fn upload(args: UploadArgs) -> anyhow::Result<()> {
    let platform = Platform::from_env()?;
    let triple = crate::env_verbs::parse_coordinate(&args.coordinate)?;
    for client in upload_release(&platform, &triple, &args.release, &args.bucket).await? {
        let state = if client.written { "written" } else { "present" };
        println!("{} {} {state}", client.package, client.location);
    }
    Ok(())
}
