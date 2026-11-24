//! Build and publish one application's source before the shared deployment path.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use wamn_component_policy::PackageManifest;
use wamn_environment::environment::document::EnvironmentDocument;
use wamn_package::PackageName;
use wamn_package::manifest::BaseDependencyRequirement;
use wamn_package::manifest::PackageIdentity;
use wamn_package::manifest::source_name::SourceName;
use wamn_release::release_package::RegistryPackagePublisher;

use anyhow::{Context as _, ensure};

/// The version policy used to build an authored package.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum BuildMode {
    Published,
    Development,
}

/// Prepare authored packages and read or build their verified output.
pub trait SourceBuild: Sync {
    type Prepared: PackageSource + Send + Sync;

    fn prepare(&self, root: &Path) -> impl Future<Output = anyhow::Result<Self::Prepared>> + Send;

    fn build(
        &self,
        source: &Self::Prepared,
        mode: BuildMode,
    ) -> impl Future<Output = anyhow::Result<PathBuf>> + Send;

    fn inputs(&self, source: &Self::Prepared) -> anyhow::Result<BTreeSet<PathBuf>>;
    fn source_dependency(&self, root: &Path, id: &PackageName) -> anyhow::Result<PathBuf>;
    fn built_root(&self, root: &Path) -> PathBuf;
    fn built_dependency(&self, parent: &Path, id: &PackageName) -> PathBuf;
    fn read_built(&self, root: &Path) -> anyhow::Result<PackageManifest>;
}

/// Build and publish source using its committed database evidence.
pub async fn publish_from_env(
    build: &impl SourceBuild,
    publisher: &RegistryPackagePublisher<'_>,
    document: &EnvironmentDocument,
    root: &Path,
) -> anyhow::Result<(wamn_registry::ApplicationName, wamn_digest::Digest)> {
    let (application, sources) = sources(build, document, root).await?;
    build_and_publish(build, publisher, application, sources, BuildMode::Published).await
}

/// Build and publish the source documents already compiled for this dev pass.
pub(crate) async fn publish_development<B: SourceBuild>(
    build: &B,
    publisher: &RegistryPackagePublisher<'_>,
    application: wamn_registry::ApplicationName,
    sources: Vec<B::Prepared>,
) -> anyhow::Result<(wamn_registry::ApplicationName, wamn_digest::Digest)> {
    build_and_publish(
        build,
        publisher,
        application,
        sources,
        BuildMode::Development,
    )
    .await
}

async fn build_and_publish<B: SourceBuild>(
    build: &B,
    publisher: &RegistryPackagePublisher<'_>,
    application: wamn_registry::ApplicationName,
    sources: Vec<B::Prepared>,
    mode: BuildMode,
) -> anyhow::Result<(wamn_registry::ApplicationName, wamn_digest::Digest)> {
    let mut roots = Vec::with_capacity(sources.len());
    for source in sources {
        roots.push(build.build(&source, mode).await?);
    }
    let digest = publisher.publish(roots).await?;
    Ok((application, digest))
}

/// Publish the selected package's existing build output without rebuilding.
pub async fn publish_built_from_env(
    build: &impl SourceBuild,
    publisher: &RegistryPackagePublisher<'_>,
    root: &Path,
) -> anyhow::Result<wamn_digest::Digest> {
    let root = build.built_root(&root.canonicalize()?);
    let parent = root
        .parent()
        .context("build directory has no parent")?
        .to_owned();
    let read = |root: PathBuf| async move {
        let manifest = build.read_built(&root)?;
        Ok(BuiltSource { root, manifest })
    };
    let roots = ordered_sources(read(root).await?, read, |id| {
        Ok(build.built_dependency(&parent, id))
    })
    .await?
    .into_iter()
    .map(|source| source.root)
    .collect();
    publisher.publish(roots).await
}

/// Resolve and compile the source closure in dependency order.
pub(crate) async fn sources<B: SourceBuild>(
    build: &B,
    document: &EnvironmentDocument,
    root: &Path,
) -> anyhow::Result<(wamn_registry::ApplicationName, Vec<B::Prepared>)> {
    let root = root
        .canonicalize()
        .context("resolve selected package root")?;
    let application = selected_application(document)?.clone();
    let sources = ordered_sources(
        build.prepare(&root).await?,
        |root| async move { build.prepare(&root).await },
        |id| build.source_dependency(&root, id),
    )
    .await?;
    Ok((application, sources))
}

fn selected_application(
    document: &EnvironmentDocument,
) -> anyhow::Result<&wamn_registry::ApplicationName> {
    ensure!(
        document.applications.len() == 1,
        "select one application before publishing source"
    );
    Ok(document
        .applications
        .keys()
        .next()
        .expect("one application"))
}

/// The coordinate and authored base dependencies used to order a source closure.
pub trait PackageSource {
    fn root(&self) -> &Path;
    fn package(&self) -> &PackageIdentity;
    fn bases(&self) -> &BTreeMap<SourceName, BaseDependencyRequirement>;
}

struct BuiltSource {
    root: PathBuf,
    manifest: PackageManifest,
}

impl PackageSource for BuiltSource {
    fn root(&self) -> &Path {
        &self.root
    }

    fn package(&self) -> &PackageIdentity {
        &self.manifest.package
    }

    fn bases(&self) -> &BTreeMap<SourceName, BaseDependencyRequirement> {
        &self.manifest.base_dependencies
    }
}

async fn ordered_sources<T: PackageSource, F: Future<Output = anyhow::Result<T>>>(
    source: T,
    read: impl Fn(PathBuf) -> F,
    base_root: impl Fn(&PackageName) -> anyhow::Result<PathBuf>,
) -> anyhow::Result<Vec<T>> {
    let mut sources = BTreeMap::from([(source.package().id.clone(), source)]);
    loop {
        let missing = sources
            .values()
            .flat_map(|source| source.bases().values())
            .find(|dependency| !sources.contains_key(&dependency.package))
            .map(|dependency| dependency.package.clone());
        let Some(id) = missing else { break };
        let source = read(base_root(&id)?).await?;
        ensure!(
            source.package().id == id,
            "source directory does not declare package {id}"
        );
        sources.insert(id, source);
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut order = Vec::with_capacity(sources.len());
    for id in sources.keys() {
        visit(id, &sources, &mut visiting, &mut visited, &mut order)?;
    }
    let order = order.into_iter().cloned().collect::<Vec<_>>();
    order
        .into_iter()
        .map(|id| sources.remove(&id).context("package dependency order"))
        .collect()
}

fn visit<'a, T: PackageSource>(
    id: &'a PackageName,
    sources: &'a BTreeMap<PackageName, T>,
    visiting: &mut BTreeSet<&'a PackageName>,
    visited: &mut BTreeSet<&'a PackageName>,
    order: &mut Vec<&'a PackageName>,
) -> anyhow::Result<()> {
    if visited.contains(id) {
        return Ok(());
    }
    ensure!(visiting.insert(id), "cyclic source dependency at {id}");
    for dependency in sources[id].bases().values() {
        let base = sources
            .get(&dependency.package)
            .context("missing base source")?;
        ensure!(
            base.package().version == dependency.version,
            "source version differs from dependency {}@{}",
            dependency.package,
            dependency.version
        );
        visit(&dependency.package, sources, visiting, visited, order)?;
    }
    visiting.remove(id);
    visited.insert(id);
    order.push(id);
    Ok(())
}
