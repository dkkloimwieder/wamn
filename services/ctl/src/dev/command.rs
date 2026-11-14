//! Command-line inputs and signals for watched product deployment.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;
use tokio::signal::unix::{SignalKind, signal};
use wamn_dev::dev::watch::{FilesystemInvalidationSource, GitSource};
use wamn_environment::environment::document::EnvironmentDocument;

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
}

/// Run the same source deployment as `wamn deploy` on each watch pass.
/// A deployed release stays available after this command exits.
pub async fn run(args: DevCommandArgs) -> anyhow::Result<()> {
    crate::operation_verbs::ControlHost::from_env()?;
    let document = EnvironmentDocument::compile(&args.config)?;
    let (_, roots) = wamn_dev::source_deploy::sources(&document, &args.overlay_root)?;
    let source = if args.watch {
        let git = GitSource::discover(&args.overlay_root).await?;
        let (shared, files) = watch_inputs(&roots, &args.config)?;
        let mut source = FilesystemInvalidationSource::new(roots, shared, git).await?;
        source.replace_configuration_files(files)?;
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
    let result = wamn_dev::dev::run(source, receiver, async |source| {
        let result = async {
            let document = EnvironmentDocument::compile(&args.config)?;
            let (application, roots) =
                wamn_dev::source_deploy::sources(&document, &args.overlay_root)?;
            if let Some(source) = source {
                let (shared, files) = watch_inputs(&roots, &args.config)?;
                source.replace_roots(roots, shared).await?;
                source.replace_configuration_files(files)?;
            }
            crate::application_verbs::develop(crate::application_verbs::DeployArgs {
                package: Some(args.overlay_root.clone()),
                release: None,
                application,
                env: args.config.clone(),
            })
            .await
        }
        .await;
        if args.watch
            && let Err(error) = &result
        {
            eprintln!("{error:#}");
        }
        result
    })
    .await
    .context("run development deployment");
    signals.abort();
    result
}

/// Inputs for publication of existing source build output.
#[derive(Debug, Args)]
pub struct PublishArgs {
    /// Source package whose complete dependency closure has been built.
    package: PathBuf,
    /// Authored document selecting one application.
    #[arg(long)]
    env: PathBuf,
}

/// Publish existing build output and print the application and release digest.
pub async fn publish(args: PublishArgs) -> anyhow::Result<()> {
    let document = EnvironmentDocument::compile(&args.env)?;
    let (application, digest) =
        wamn_dev::source_deploy::publish_built_from_env(&document, &args.package).await?;
    println!("{application} {digest}");
    Ok(())
}

fn watch_inputs(
    roots: &[PathBuf],
    configuration: &std::path::Path,
) -> anyhow::Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut inputs = std::collections::BTreeSet::new();
    for root in roots {
        inputs.extend(wamn_dev::source_deploy::build_inputs(root)?);
    }
    inputs.insert(configuration.canonicalize()?);
    Ok(inputs.into_iter().partition(|path| path.is_dir()))
}
