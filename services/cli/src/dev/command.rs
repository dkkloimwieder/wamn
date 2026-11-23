//! Command-line inputs and signals for watched product deployment.

use std::path::PathBuf;
use wamn_postgres::build_database::PostgresBuildDatabase;

use anyhow::Context as _;
use clap::Args;
use tokio::signal::unix::{SignalKind, signal};
use wamn_dev::dev::watch::{FilesystemInvalidationSource, GitSource};
use wamn_environment::environment::document::{EnvironmentDocument, EnvironmentFile};

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
    let document = source_project(&args.config)?;
    let mut first = Some(wamn_dev::source_deploy::sources(&document, &args.overlay_root).await?);
    let source = if args.watch {
        let git = GitSource::discover(&args.overlay_root).await?;
        let (_, sources) = first.as_ref().context("initial source pass")?;
        let roots = sources
            .iter()
            .map(|source| source.root().to_owned())
            .collect::<Vec<_>>();
        let (shared, files) = watch_inputs(sources, &args.config)?;
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
            let initial = first.is_some();
            let (application, sources) = if let Some(pass) = first.take() {
                pass
            } else {
                let document = source_project(&args.config)?;
                wamn_dev::source_deploy::sources(&document, &args.overlay_root).await?
            };
            if let Some(source) = source
                && !initial
            {
                let roots = sources
                    .iter()
                    .map(|source| source.root().to_owned())
                    .collect::<Vec<_>>();
                let (shared, files) = watch_inputs(&sources, &args.config)?;
                source.replace_roots(roots, shared).await?;
                source.replace_configuration_files(files)?;
            }
            let (application, digest) = wamn_dev::source_deploy::publish_development(
                &PostgresBuildDatabase,
                application,
                sources,
                args.describe.mode(false)?,
            )
            .await?;
            crate::application_verbs::deploy(crate::application_verbs::DeployArgs {
                package: None,
                release: Some(digest),
                application,
                env: args.config.clone(),
                describe: RefreshDescribeArgs::default(),
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
    /// Build directory whose package dependency closure has been built.
    package: PathBuf,
}

/// Publish existing build output and print the top package OCI digest.
pub async fn publish(args: PublishArgs) -> anyhow::Result<()> {
    let digest = wamn_dev::source_deploy::publish_built_from_env(&args.package).await?;
    println!("{digest}");
    Ok(())
}

fn source_project(file: &std::path::Path) -> anyhow::Result<EnvironmentDocument> {
    let document = EnvironmentFile::compile(file)?;
    let application = document.only_application()?.clone();
    document.into_application(&application)
}

fn watch_inputs(
    sources: &[wamn_build::PreparedSource],
    configuration: &std::path::Path,
) -> anyhow::Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut inputs = std::collections::BTreeSet::new();
    for source in sources {
        inputs.extend(wamn_build::build_inputs(source)?);
    }
    inputs.insert(configuration.canonicalize()?);
    Ok(inputs.into_iter().partition(|path| path.is_dir()))
}
