//! Build, push, and upload web clients (docs/operations/web-upload.md).

use anyhow::Context as _;
use clap::{Args, Subcommand};
use wamn_environment_control::platform::web::{PLATFORM_UI_BUCKET, upload_platform_ui};
use wamn_package_control::web_artifact::{self, upload_web_artifact};

/// Web client commands.
#[derive(Debug, Args)]
pub struct WebArgs {
    #[command(subcommand)]
    pub command: WebCommand,
}

#[derive(Debug, Subcommand)]
pub enum WebCommand {
    /// Build a package's web client and write web.json beside dist.
    Build(BuildArgs),
    /// Push a built web client as an immutable OCI artifact.
    Push(PushArgs),
    /// Upload a pinned web artifact or the local platform UI.
    Upload(UploadArgs),
}

#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Source package whose build output holds build.json and wamn.json.
    #[arg(long)]
    pub package: std::path::PathBuf,
}

#[derive(Debug, Args)]
pub struct PushArgs {
    /// Source package whose web-artifact directory holds dist and web.json.
    #[arg(long)]
    pub package: std::path::PathBuf,
    #[command(flatten)]
    pub registry: crate::package_verbs::RegistryArgs,
}

#[derive(Debug, Args)]
pub struct UploadArgs {
    /// The web artifact digest, `sha256:` and 64 lowercase hexadecimal digits.
    #[arg(
        long,
        required_unless_present = "platform_ui",
        conflicts_with = "platform_ui",
        requires = "RegistryArgs"
    )]
    pub web: Option<String>,
    #[command(flatten)]
    pub registry: Option<crate::package_verbs::RegistryArgs>,
    /// The bucket and an optional prefix, `s3://<bucket>[/<prefix>]` or
    /// `gs://<bucket>[/<prefix>]`. For `s3://` the `AWS_*` variables supply
    /// the endpoint and the credentials. For `gs://` the `GOOGLE_*` variables
    /// and Application Default Credentials do.
    #[arg(long, required_unless_present = "platform_ui")]
    pub bucket: Option<String>,
    /// Upload apps/platform_ui/dist using WAMN_RELEASE_CHART's build record.
    #[arg(long, conflicts_with = "RegistryArgs")]
    pub platform_ui: bool,
    /// The org written to config.json.
    #[arg(long)]
    pub org: String,
}

pub async fn run(args: WebArgs) -> anyhow::Result<()> {
    match args.command {
        WebCommand::Build(args) => {
            let output = web_artifact::build_web_artifact(&args.package).await?;
            println!("{} built", output.display());
            Ok(())
        }
        WebCommand::Push(args) => {
            let root = web_artifact::artifact_output(&args.package);
            let packed = web_artifact::pack_web_artifact(&root)?;
            let pushed =
                web_artifact::push_web_artifact(&packed, &args.registry.registry()).await?;
            println!("{} {} {:?}", pushed.tag, pushed.digest, pushed.disposition);
            Ok(())
        }
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
            &args.org,
            args.bucket.as_deref().unwrap_or(PLATFORM_UI_BUCKET),
        )
        .await?;
        println!("platform-ui {location} written");
        return Ok(());
    }
    let uploaded = upload_web_artifact(
        &args
            .registry
            .context("--web requires registry arguments")?
            .registry(),
        args.web.as_deref().context("upload requires --web")?,
        &args.org,
        args.bucket.as_deref().context("--web requires --bucket")?,
    )
    .await?;
    println!(
        "{} {} {}",
        uploaded.package,
        uploaded.location,
        if uploaded.written {
            "written"
        } else {
            "present"
        }
    );
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
    fn build_and_push_require_their_own_inputs() {
        assert!(Cli::try_parse_from(["web", "build", "--package", "apps/client"]).is_ok());
        assert!(Cli::try_parse_from(["web", "build"]).is_err());
        assert!(
            Cli::try_parse_from([
                "web",
                "push",
                "--package",
                "apps/client",
                "--artifact-base",
                "registry.example/web",
                "--registry-auth-file",
                "auth.json",
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["web", "push", "--package", "apps/client"]).is_err());
    }

    #[test]
    fn upload_requires_a_web_digest_org_bucket_and_registry() {
        let valid = [
            "web",
            "upload",
            "--web",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--org",
            "acme",
            "--bucket",
            "gs://web/prefix",
            "--artifact-base",
            "registry.example/packages",
            "--registry-auth-file",
            "auth.json",
            "--insecure-registry",
            "--oci-ca-path",
            "ca.pem",
        ];
        assert!(Cli::try_parse_from(valid).is_ok());
        for flag in [
            "--web",
            "--org",
            "--bucket",
            "--artifact-base",
            "--registry-auth-file",
        ] {
            let index = valid.iter().position(|value| *value == flag).unwrap();
            let mut missing = valid.to_vec();
            missing.drain(index..index + 2);
            assert!(Cli::try_parse_from(missing).is_err(), "missing {flag}");
        }
        assert!(
            Cli::try_parse_from([
                "web",
                "upload",
                "acme/receiving/dev",
                "--release",
                "sha256:abc",
                "--bucket",
                "gs://web"
            ])
            .is_err()
        );
    }

    #[test]
    fn platform_ui_keeps_its_own_inputs() {
        assert!(Cli::try_parse_from(["web", "upload", "--platform-ui", "--org", "dkk"]).is_ok());
        assert!(Cli::try_parse_from(["web", "upload", "--platform-ui"]).is_err());
        assert!(Cli::try_parse_from(["web", "upload", "--org", "dkk"]).is_err());
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
        assert!(
            Cli::try_parse_from([
                "web",
                "upload",
                "--platform-ui",
                "--org",
                "dkk",
                "--web",
                "sha256:abc"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "web",
                "upload",
                "--platform-ui",
                "--org",
                "dkk",
                "--artifact-base",
                "r/packages",
                "--registry-auth-file",
                "auth.json"
            ])
            .is_err()
        );
    }
}
