//! Source preparation and repeatable development deployment.

use std::collections::BTreeSet;
use std::path::Path;
use wamn_environment::environment::document::EnvironmentFile;
use wamn_registry::ApplicationName;
use wamn_release::package_artifact::PackagePublisher;

use super::{Deploy, DevInvalidationSource, WatchInputs};
use crate::source_deploy::{self, PackageSource, SourceBuild};

/// The compiled first pass and the inputs used to prepare subsequent passes.
#[derive(Debug)]
pub struct Development<'a, B: SourceBuild> {
    build: &'a B,
    configuration: &'a Path,
    root: &'a Path,
    first: (ApplicationName, Vec<B::Prepared>),
}

impl<'a, B: SourceBuild> Development<'a, B> {
    /// Read the selected application and prepare its source closure.
    pub async fn prepare(
        build: &'a B,
        configuration: &'a Path,
        root: &'a Path,
    ) -> anyhow::Result<Self> {
        let first = prepare(build, configuration, root).await?;
        Ok(Self {
            build,
            configuration,
            root,
            first,
        })
    }

    /// Inputs needed before the first pass starts.
    pub fn watch_inputs(&self) -> anyhow::Result<WatchInputs> {
        watch_inputs(self.build, &self.first.1, self.configuration)
    }

    /// Publish and deploy each selected source closure.
    pub async fn run<S: DevInvalidationSource>(
        self,
        source: Option<S>,
        shutdown: tokio::sync::watch::Receiver<bool>,
        publisher: &dyn PackagePublisher,
        deployer: &impl Deploy,
        report_retry: impl Fn(&anyhow::Error),
    ) -> anyhow::Result<()> {
        let Self {
            build,
            configuration,
            root,
            first,
        } = self;
        let watching = source.is_some();
        let mut first = Some(first);
        super::run(source, shutdown, async |source| {
            let result = async {
                let (application, sources) = if let Some(pass) = first.take() {
                    pass
                } else {
                    let pass = prepare(build, configuration, root).await?;
                    if let Some(source) = source {
                        source
                            .replace_inputs(watch_inputs(build, &pass.1, configuration)?)
                            .await?;
                    }
                    pass
                };
                let (application, digest) =
                    source_deploy::publish_development(build, publisher, application, sources)
                        .await?;
                deployer.deploy(configuration, application, digest).await
            }
            .await;
            if watching && let Err(error) = &result {
                report_retry(error);
            }
            result
        })
        .await
    }
}

async fn prepare<B: SourceBuild>(
    build: &B,
    configuration: &Path,
    root: &Path,
) -> anyhow::Result<(ApplicationName, Vec<B::Prepared>)> {
    let document = EnvironmentFile::compile(configuration)?;
    let application = document.only_application()?.clone();
    let project = document.into_application(&application)?;
    source_deploy::sources(build, &project, root).await
}

fn watch_inputs<B: SourceBuild>(
    build: &B,
    sources: &[B::Prepared],
    configuration: &Path,
) -> anyhow::Result<WatchInputs> {
    let mut inputs = BTreeSet::new();
    for source in sources {
        inputs.extend(build.inputs(source)?);
    }
    inputs.insert(configuration.canonicalize()?);
    let (shared, files) = inputs.into_iter().partition(|path| path.is_dir());
    Ok(WatchInputs {
        roots: sources
            .iter()
            .map(|source| source.root().to_owned())
            .collect(),
        shared,
        files,
    })
}
