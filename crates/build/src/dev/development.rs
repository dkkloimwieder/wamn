//! Source preparation and repeatable development deployment.

use std::collections::BTreeSet;
use std::path::Path;
use wamn_environment::environment::document::EnvironmentFile;
use wamn_registry::ApplicationName;
use wamn_release::release_package::RegistryPackagePublisher;

use super::{Deploy, WatchInputs};
use crate::source_build::{PreparedPackage, SourceBuilder};
use crate::source_deploy::{self, PackageSource};
use crate::watch::FilesystemInvalidationSource;

/// The compiled first pass and the inputs used to prepare subsequent passes.
#[derive(Debug)]
pub struct Development<'a> {
    build: &'a SourceBuilder<'a>,
    configuration: &'a Path,
    root: &'a Path,
    first: (ApplicationName, Vec<PreparedPackage>),
}

impl<'a> Development<'a> {
    /// Read the selected application and prepare its source closure.
    pub async fn prepare(
        build: &'a SourceBuilder<'a>,
        configuration: &'a Path,
        root: &'a Path,
    ) -> anyhow::Result<Self> {
        let first = prepare(configuration, root).await?;
        Ok(Self {
            build,
            configuration,
            root,
            first,
        })
    }

    /// Inputs needed before the first pass starts.
    pub fn watch_inputs(&self) -> anyhow::Result<WatchInputs> {
        watch_inputs(&self.first.1, self.configuration)
    }

    /// Publish and deploy each selected source closure.
    pub async fn run(
        self,
        source: Option<FilesystemInvalidationSource>,
        shutdown: tokio::sync::watch::Receiver<bool>,
        publisher: &RegistryPackagePublisher<'_>,
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
                    let pass = prepare(configuration, root).await?;
                    if let Some(source) = source {
                        source
                            .replace_inputs(watch_inputs(&pass.1, configuration)?)
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

async fn prepare(
    configuration: &Path,
    root: &Path,
) -> anyhow::Result<(ApplicationName, Vec<PreparedPackage>)> {
    let document = EnvironmentFile::compile(configuration)?;
    let application = document.only_application()?.clone();
    let project = document.into_application(&application)?;
    source_deploy::sources(&project, root).await
}

fn watch_inputs(sources: &[PreparedPackage], configuration: &Path) -> anyhow::Result<WatchInputs> {
    let mut inputs = BTreeSet::new();
    for source in sources {
        inputs.extend(source.inputs()?);
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
