//! Command-line inputs and signals for watched product deployment.

use std::path::{Path, PathBuf};
use wamn_build::dev::{Deploy, development::Development};
use wamn_build::source_build::SourceBuilder;
use wamn_build::watch::{FilesystemInvalidationSource, GitSource};

use anyhow::Context as _;
use clap::Args;
use tokio::signal::unix::{SignalKind, signal};

use crate::build_verbs::RefreshDescribeArgs;

/// Inputs for development against an installed platform.
#[derive(Debug, Args)]
pub struct DevCommandArgs {
    /// Authored environment document on the development cluster.
    #[arg(long, value_name = "FILE")]
    config: PathBuf,
    /// Source package for the selected application.
    #[arg(long, value_name = "DIRECTORY")]
    overlay_root: PathBuf,
    /// Rebuild, publish and deploy when source inputs change.
    #[arg(long)]
    watch: bool,
    #[command(flatten)]
    describe: RefreshDescribeArgs,
}

/// Run the same source deployment as `wamn deploy` on each watch pass.
/// A deployed release stays available after this command exits.
pub async fn run(args: DevCommandArgs) -> anyhow::Result<()> {
    crate::operation_verbs::ControlHost::from_env()?;
    let build = SourceBuilder {
        describe: args.describe.mode(false),
    };
    let development = Development::prepare(&build, &args.config, &args.overlay_root).await?;
    let source = if args.watch {
        let git = GitSource::discover(&args.overlay_root).await?;
        let inputs = development.watch_inputs()?;
        let mut source =
            FilesystemInvalidationSource::new(inputs.roots, inputs.shared, git).await?;
        source.replace_configuration_files(inputs.files)?;
        Some(source)
    } else {
        None
    };
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let signals = tokio::spawn(async move {
        tokio::select! {
            _ = interrupt.recv() => {},
            _ = terminate.recv() => {},
            _ = hangup.recv() => {},
        }
        shutdown.send_replace(true);
    });
    let result = development
        .run(
            source,
            receiver,
            &crate::package_verbs::package_publisher(),
            &ControlDeployment,
            |error| eprintln!("{error:#}"),
        )
        .await
        .context("run development deployment");
    signals.abort();
    result
}

struct ControlDeployment;

impl Deploy for ControlDeployment {
    async fn deploy(
        &self,
        configuration: &Path,
        app: wamn_registry::AppName,
        release: wamn_digest::Digest,
    ) -> anyhow::Result<()> {
        crate::app_verbs::deploy(crate::app_verbs::DeployArgs {
            package: None,
            release: Some(release),
            app,
            env: configuration.to_owned(),
            describe: RefreshDescribeArgs::default(),
        })
        .await
    }
}

/// Inputs for publication of existing source build output.
#[derive(Debug, Args)]
pub struct PublishArgs {
    /// Build directory whose package dependency closure has been built.
    package: PathBuf,
}

/// Publish existing build output and print the top package OCI digest.
pub async fn publish(args: PublishArgs) -> anyhow::Result<()> {
    let digest = wamn_build::source_deploy::publish_built_from_env(
        &crate::package_verbs::package_publisher(),
        &args.package,
    )
    .await?;
    println!("{digest}");
    Ok(())
}
