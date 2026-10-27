//! Web client files in a bucket (docs/plan/web-deployment.md).
//!
//! `wamn_control::web_upload` writes the built client of each package of the
//! release that an environment serves, from its package artifact.

use anyhow::Context as _;
use clap::{Args, Subcommand};
use wamn_control::environment::Platform;
use wamn_control::web_upload::{upload_platform_ui, upload_release};

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
    #[arg(
        required_unless_present = "platform_ui",
        conflicts_with = "platform_ui"
    )]
    pub coordinate: Option<String>,
    /// The release the files belong to: the manifest digest, `sha256:` and 64
    /// lowercase hexadecimal digits. The upload refuses a release that the
    /// environment does not serve.
    #[arg(
        long,
        required_unless_present = "platform_ui",
        conflicts_with = "platform_ui"
    )]
    pub release: Option<String>,
    /// The bucket and an optional prefix, `s3://<bucket>[/<prefix>]` or
    /// `gs://<bucket>[/<prefix>]`. For `s3://` the `AWS_*` variables supply
    /// the endpoint and the credentials. For `gs://` the `GOOGLE_*` variables
    /// and Application Default Credentials do.
    #[arg(long, required_unless_present = "platform_ui")]
    pub bucket: Option<String>,
    /// Upload apps/platform_ui/dist using WAMN_RELEASE_CHART's build receipt.
    #[arg(long, requires = "org")]
    pub platform_ui: bool,
    /// The org written to the platform UI config.json.
    #[arg(long, requires = "platform_ui")]
    pub org: Option<String>,
}

// Environment contract for this verb as invoked by platform install Jobs.
#[cfg(test)]
pub(crate) const WEB_UPLOAD_INSTALL_ENV: crate::install_job_tests::Environment =
    crate::install_job_tests::Environment {
        required: &[
            "WAMN_CONTROL_DATABASE_URL",
            "WAMN_RELEASE_CHART",
            "WAMN_RELEASE_ARTIFACT_BASE",
            "WAMN_REGISTRY_AUTH_FILE",
            "WAMN_PACKAGE_ARTIFACT_BASE",
            "WAMN_AUTH_NATS_URL",
            "WAMN_AUTH_NATS_TLS_CA",
            "WAMN_AUTH_NATS_TLS_CERT",
            "WAMN_AUTH_NATS_TLS_KEY",
        ],
        optional: &[
            "KUBECONFIG",
            "WAMN_RELEASE_NAMESPACE",
            "WAMN_OCI_CA_PATHS",
            "WAMN_AUTH_NATS_TLS_SERVER_NAME",
        ],
    };

#[cfg(test)]
pub(crate) const PLATFORM_UI_INSTALL_ENV: crate::install_job_tests::Environment =
    crate::install_job_tests::Environment {
        required: &["WAMN_RELEASE_CHART"],
        optional: &[],
    };

pub async fn run(args: WebArgs) -> anyhow::Result<()> {
    match args.command {
        WebCommand::Upload(args) => upload(args).await,
    }
}

async fn upload(args: UploadArgs) -> anyhow::Result<()> {
    if args.platform_ui {
        let chart = std::env::var_os("WAMN_RELEASE_CHART")
            .context("set WAMN_RELEASE_CHART to the stamped platform chart")?;
        let location = upload_platform_ui(
            &std::env::current_dir()?,
            std::path::Path::new(&chart),
            args.org
                .as_deref()
                .context("--platform-ui requires --org")?,
            args.bucket
                .as_deref()
                .unwrap_or(wamn_control::web_upload::PLATFORM_UI_BUCKET),
        )
        .await?;
        println!("platform-ui {location} written");
        return Ok(());
    }
    let platform = Platform::from_env()?;
    let triple = crate::env_verbs::parse_coordinate(
        args.coordinate
            .as_deref()
            .context("a package upload requires a coordinate")?,
    )?;
    for client in upload_release(
        &platform,
        &triple,
        args.release
            .as_deref()
            .context("a package upload requires --release")?,
        args.bucket
            .as_deref()
            .context("a package upload requires --bucket")?,
    )
    .await?
    {
        let state = if client.written { "written" } else { "present" };
        println!("{} {} {state}", client.package, client.location);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        web: WebArgs,
    }

    #[test]
    fn upload_sources_have_separate_required_inputs() {
        assert!(Cli::try_parse_from(["web", "upload", "--platform-ui", "--org", "dkk"]).is_ok());
        assert!(Cli::try_parse_from(["web", "upload", "--platform-ui"]).is_err());
        assert!(Cli::try_parse_from(["web", "upload", "--org", "dkk"]).is_err());
        assert!(
            Cli::try_parse_from([
                "web",
                "upload",
                "dkk/wms/dev",
                "--release",
                "sha256:abc",
                "--bucket",
                "gs://web"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "web",
                "upload",
                "--platform-ui",
                "--org",
                "dkk",
                "--bucket",
                "gs://other"
            ])
            .is_ok()
        );
    }
}
